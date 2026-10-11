//! 心跳：每拍零 token——读信号、算压力、决定要不要点火（docs/mind-design.md §3.1）。

use crate::core::mind::Mind;
use crate::core::ports::{Clock, MemoryPort, Metabolism, Expression};
use crate::core::stream::Percept;
use crate::heart::{Drive, Signals};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 心跳参数（`[mind.heart]`）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct HeartConfig {
    /// 每拍间隔（秒）。
    pub interval_secs: u64,
    /// 点火阈值（压力 0–1）。
    pub threshold: f32,
    /// 两次点火的最小间隔（秒），防抖。
    pub min_gap_secs: u64,
    /// 疲劳度对阈值的抬升：阈值 = threshold + fatigue × fatigue_gain（封顶 0.99）。
    pub fatigue_gain: f32,
    /// 回合结束后多久内不点火（让她先"消化"刚发生的事），秒。
    pub settle_secs: u64,
}

impl Default for HeartConfig {
    fn default() -> Self {
        Self { interval_secs: 5, threshold: 0.6, min_gap_secs: 120, fatigue_gain: 0.35, settle_secs: 30 }
    }
}

/// 每个驱动此刻的压力（给 `/api/mind/status` 和"心"页）。
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct DriveReading {
    pub name: String,
    pub kind: crate::heart::DriveKind,
    pub pressure: f32,
    pub reason: Option<String>,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
pub struct HeartState {
    pub readings: Vec<DriveReading>,
    pub threshold: f32,
    pub last_fired_ms: i64,
    pub last_fired: Option<String>,
    /// 每个驱动最近一次点火（一个驱动点过火后，在最小间隔内不重复点）。
    pub fired_ms: BTreeMap<String, i64>,
    pub last_satisfied_ms: BTreeMap<String, i64>,
}

/// 心跳句柄：读状态。
#[derive(Clone)]
pub struct Heart {
    state: Arc<Mutex<HeartState>>,
}

pub struct HeartPorts {
    pub memory: Option<Arc<dyn MemoryPort>>,
    pub metabolism: Arc<dyn Metabolism>,
    pub expression: Arc<dyn Expression>,
    pub clock: Arc<dyn Clock>,
    /// 认识的人的 id（判断谁在线）。
    pub people: Arc<dyn crate::core::ports::People>,
}

impl Heart {
    pub fn state(&self) -> HeartState {
        self.state.lock().expect("heart poisoned").clone()
    }

    /// 启动心跳。`last_satisfied_ms` 用于从持久化恢复（缺省全 0）。
    pub fn start(
        mind: Mind,
        drives: Vec<Box<dyn Drive>>,
        config: HeartConfig,
        ports: HeartPorts,
        last_satisfied_ms: BTreeMap<String, i64>,
    ) -> Self {
        let state = Arc::new(Mutex::new(HeartState { threshold: config.threshold, last_satisfied_ms, ..Default::default() }));
        let heart = Self { state: Arc::clone(&state) };
        let drives: Arc<Vec<Box<dyn Drive>>> = Arc::new(drives);
        let last_turn_end = Arc::new(Mutex::new(0i64));

        // 回合结果 → 满足判断
        {
            let (state, drives, clock, last_turn_end) = (Arc::clone(&state), Arc::clone(&drives), Arc::clone(&ports.clock), Arc::clone(&last_turn_end));
            let mut outcomes = mind.outcomes();
            tokio::spawn(async move {
                while let Ok((fired, outcome)) = outcomes.recv().await {
                    let now = clock.now_ms();
                    *last_turn_end.lock().expect("poisoned") = now;
                    let mut st = state.lock().expect("heart poisoned");
                    for d in drives.iter() {
                        // 点火的那个驱动按满足函数判断；其他驱动若顺带被满足也记下（比如对话里聊到了想分享的事）
                        let relevant = fired.as_deref() == Some(d.name()) || fired.is_none();
                        if relevant && d.satisfied(&outcome) {
                            st.last_satisfied_ms.insert(d.name().to_owned(), now);
                        }
                    }
                }
            });
        }

        // 心跳
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(config.interval_secs.max(1)));
            loop {
                tick.tick().await;
                let now = ports.clock.now_ms();
                let Ok(self_model) = mind.self_model().await else { return };
                let untidiness = match &ports.memory {
                    Some(m) => m.untidiness().await.unwrap_or(0),
                    None => 0,
                };
                let present = match ports.people.all().await {
                    Ok(all) => all.into_iter().filter(|p| ports.expression.is_present(&p.id)).map(|p| p.id).collect(),
                    Err(_) => Vec::new(),
                };
                let fatigue = ports.metabolism.fatigue();
                let last_satisfied_ms = state.lock().expect("heart poisoned").last_satisfied_ms.clone();
                let signals = Signals { now_ms: now, self_model, untidiness, fatigue, last_satisfied_ms, present };

                let readings: Vec<DriveReading> = drives
                    .iter()
                    .map(|d| {
                        let urge = d.urge(&signals);
                        DriveReading {
                            name: d.name().to_owned(),
                            kind: d.kind(),
                            pressure: urge.as_ref().map_or(0.0, |u| u.pressure),
                            reason: urge.map(|u| u.reason),
                        }
                    })
                    .collect();
                let threshold = (config.threshold + fatigue as f32 * config.fatigue_gain).min(0.99);

                let fire = {
                    let mut st = state.lock().expect("heart poisoned");
                    st.readings = readings.clone();
                    st.threshold = threshold;
                    let quiet_since_turn = now - *last_turn_end.lock().expect("poisoned") >= config.settle_secs as i64 * 1000;
                    let gap_ok = now - st.last_fired_ms >= config.min_gap_secs as i64 * 1000;
                    let candidate = readings
                        .iter()
                        .filter(|r| r.pressure >= threshold)
                        // 刚被满足过 / 刚点过火的驱动在最小间隔内不再点火
                        .filter(|r| {
                            let gap = config.min_gap_secs as i64 * 1000;
                            now - st.last_satisfied_ms.get(&r.name).copied().unwrap_or(0) >= gap
                                && now - st.fired_ms.get(&r.name).copied().unwrap_or(0) >= gap.max(1)
                        })
                        .max_by(|a, b| a.pressure.total_cmp(&b.pressure))
                        .cloned();
                    match candidate {
                        Some(r) if gap_ok && quiet_since_turn && !mind.is_thinking() && ports.metabolism.may_act_autonomously() => {
                            st.last_fired_ms = now;
                            st.last_fired = Some(r.name.clone());
                            st.fired_ms.insert(r.name.clone(), now);
                            Some(r)
                        }
                        _ => None,
                    }
                };
                if let Some(r) = fire {
                    mind.perceive_nowait(Percept::Drive {
                        drive: r.name,
                        pressure: r.pressure,
                        reason: r.reason.unwrap_or_default(),
                    });
                }
            }
        });
        heart
    }
}
