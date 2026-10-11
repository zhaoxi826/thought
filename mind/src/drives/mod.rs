//! 阶段 1 的驱动：一个函数一个驱动，压力完全由真实信号计算（docs/mind-design.md §3.2）。

use crate::heart::{saturate, Drive, DriveKind, Outcome, Signals, Urge};
use serde::{Deserialize, Serialize};

const HOUR: f64 = 3_600_000.0;

/// 驱动参数（`[mind.drives]`），τ 单位为小时。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct DriveParams {
    /// 想念：未见时长 τ（小时）。
    pub missing_tau_h: f64,
    /// 想念：交谈次数达到多少视为亲密度 1。
    pub missing_intimacy_at: f64,
    /// 好奇：距上次探索的 τ（小时）。
    pub curiosity_tau_h: f64,
    /// 自己的事：距上次推进的 τ（小时）。
    pub project_tau_h: f64,
    /// 整理：待整理量 τ（条）。
    pub tidy_tau: f64,
    /// 分享：想告诉某人的事条数 τ。
    pub share_tau: f64,
}

impl Default for DriveParams {
    fn default() -> Self {
        Self {
            missing_tau_h: 24.0,
            missing_intimacy_at: 20.0,
            curiosity_tau_h: 8.0,
            project_tau_h: 12.0,
            tidy_tau: 50.0,
            share_tau: 2.0,
        }
    }
}

fn since_h(now: i64, then: i64) -> f64 {
    if then <= 0 { 0.0 } else { (now - then).max(0) as f64 / HOUR }
}

fn last(signals: &Signals, drive: &str) -> i64 {
    signals.last_satisfied_ms.get(drive).copied().unwrap_or(0)
}

/// 构造全部阶段 1 驱动。
pub fn all(params: &DriveParams) -> Vec<Box<dyn Drive>> {
    vec![
        Box::new(Missing(params.clone())),
        Box::new(Share(params.clone())),
        Box::new(Curiosity(params.clone())),
        Box::new(Project(params.clone())),
        Box::new(Tidy(params.clone())),
    ]
}

// ---- 想念（关系）----

/// 很久没和某个亲近的人说话了。在线的人不算（他就在这儿）。
pub struct Missing(pub DriveParams);

impl Drive for Missing {
    fn name(&self) -> &str {
        "missing"
    }
    fn kind(&self) -> DriveKind {
        DriveKind::Relation
    }
    fn urge(&self, s: &Signals) -> Option<Urge> {
        let p = &self.0;
        s.self_model
            .people
            .iter()
            .filter(|(id, a)| a.last_seen_ms > 0 && !s.present.contains(id))
            .map(|(_, a)| {
                let intimacy = (a.exchanges as f64 / p.missing_intimacy_at).min(1.0) as f32;
                let away = since_h(s.now_ms, a.last_seen_ms);
                (saturate(away, p.missing_tau_h) * intimacy, a, away)
            })
            .filter(|(pressure, ..)| *pressure > 0.0)
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(pressure, a, away)| Urge {
                pressure,
                reason: format!("已经 {:.0} 小时没和{}说话了，有点想他。", away, a.name),
            })
    }
    fn satisfied(&self, o: &Outcome) -> bool {
        !o.said_to.is_empty()
    }
}

// ---- 分享（关系）----

/// 有想告诉某人的事（她在 people.<id>.open_threads 里记下的）。
pub struct Share(pub DriveParams);

impl Drive for Share {
    fn name(&self) -> &str {
        "share"
    }
    fn kind(&self) -> DriveKind {
        DriveKind::Relation
    }
    fn urge(&self, s: &Signals) -> Option<Urge> {
        s.self_model
            .people
            .values()
            .filter(|a| !a.open_threads.is_empty())
            .map(|a| (saturate(a.open_threads.len() as f64, self.0.share_tau), a))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(pressure, a)| Urge {
                pressure,
                reason: format!("有事想告诉{}：{}", a.name, a.open_threads.join("；")),
            })
    }
    fn satisfied(&self, o: &Outcome) -> bool {
        !o.said_to.is_empty()
    }
}

// ---- 好奇（自我）----

/// 有感兴趣但很久没探索的东西。还没有兴趣时，想去发现点什么。
pub struct Curiosity(pub DriveParams);

impl Drive for Curiosity {
    fn name(&self) -> &str {
        "curiosity"
    }
    fn kind(&self) -> DriveKind {
        DriveKind::Selfhood
    }
    fn urge(&self, s: &Signals) -> Option<Urge> {
        let tau = self.0.curiosity_tau_h;
        if s.self_model.interests.is_empty() {
            let idle = since_h(s.now_ms, last(s, "curiosity").max(s.self_model.updated_ms));
            let pressure = saturate(idle, tau) * 0.6;
            return (pressure > 0.0).then(|| Urge { pressure, reason: "还不知道自己对什么感兴趣，想去发现点什么。".into() });
        }
        s.self_model
            .interests
            .iter()
            .map(|i| {
                let then = if i.last_explored_ms > 0 { i.last_explored_ms } else { i.since_ms.max(1) };
                (saturate(since_h(s.now_ms, then), tau), i)
            })
            .filter(|(p, _)| *p > 0.0)
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(pressure, i)| Urge { pressure, reason: format!("好久没琢磨「{}」了。", i.topic) })
    }
    fn satisfied(&self, o: &Outcome) -> bool {
        o.reflected || o.tools_used.iter().any(|t| t != "say")
    }
}

// ---- 自己的事（自我）----

/// 有在做的事，且有下一步。
pub struct Project(pub DriveParams);

impl Drive for Project {
    fn name(&self) -> &str {
        "project"
    }
    fn kind(&self) -> DriveKind {
        DriveKind::Selfhood
    }
    fn urge(&self, s: &Signals) -> Option<Urge> {
        s.self_model
            .projects
            .iter()
            .filter(|p| p.status == "active" && !p.next.is_empty())
            .map(|p| (saturate(since_h(s.now_ms, p.last_progress_ms.max(1)), self.0.project_tau_h), p))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(pressure, p)| Urge { pressure, reason: format!("「{}」该推进了：{}", p.title, p.next) })
    }
    fn satisfied(&self, o: &Outcome) -> bool {
        o.reflected || o.tools_used.iter().any(|t| t != "say" && t != "journal")
    }
}

// ---- 整理（自我）----

/// 记忆里积了不少没整理的东西。
pub struct Tidy(pub DriveParams);

impl Drive for Tidy {
    fn name(&self) -> &str {
        "tidy"
    }
    fn kind(&self) -> DriveKind {
        DriveKind::Selfhood
    }
    fn urge(&self, s: &Signals) -> Option<Urge> {
        let pressure = saturate(s.untidiness as f64, self.0.tidy_tau) * 0.7;
        (pressure > 0.05).then(|| Urge {
            pressure,
            reason: format!("记忆里有 {} 处还没整理，想回顾一下最近的事、写写日记。", s.untidiness),
        })
    }
    fn satisfied(&self, o: &Outcome) -> bool {
        o.tools_used.iter().any(|t| t == "journal" || t.starts_with("memory_"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_model::{Acquaintance, Interest, Project as P};

    fn signals(now_h: f64) -> Signals {
        Signals { now_ms: (now_h * HOUR) as i64, ..Default::default() }
    }

    #[test]
    fn missing_grows_with_absence_and_intimacy_but_not_for_present_people() {
        let d = Missing(DriveParams::default());
        let mut s = signals(100.0);
        s.self_model.people.insert("u1".into(), Acquaintance { name: "zhaoxi".into(), last_seen_ms: (52.0 * HOUR) as i64, exchanges: 20, ..Default::default() });
        s.self_model.people.insert("u2".into(), Acquaintance { name: "小王".into(), last_seen_ms: (52.0 * HOUR) as i64, exchanges: 2, ..Default::default() });
        let u = d.urge(&s).unwrap();
        assert!(u.reason.contains("zhaoxi"), "closer person wins: {}", u.reason);
        assert!(u.pressure > 0.8);
        s.present.push("u1".into());
        assert!(d.urge(&s).unwrap().reason.contains("小王"));
        assert!(d.urge(&signals(1.0)).is_none(), "no one known yet");
    }

    #[test]
    fn curiosity_project_tidy_share() {
        let p = DriveParams::default();
        let mut s = signals(100.0);
        s.self_model.updated_ms = (99.0 * HOUR) as i64;
        let u = Curiosity(p.clone()).urge(&s).unwrap();
        assert!(u.pressure < 0.2 && u.reason.contains("发现"));
        s.self_model.interests.push(Interest { topic: "星星".into(), last_explored_ms: (90.0 * HOUR) as i64, ..Default::default() });
        assert!(Curiosity(p.clone()).urge(&s).unwrap().reason.contains("星星"));

        assert!(Project(p.clone()).urge(&s).is_none());
        s.self_model.projects.push(P { id: "p".into(), title: "读完一本书".into(), status: "active".into(), next: "第三章".into(), last_progress_ms: (80.0 * HOUR) as i64 });
        assert!(Project(p.clone()).urge(&s).unwrap().reason.contains("第三章"));

        assert!(Tidy(p.clone()).urge(&s).is_none());
        s.untidiness = 100;
        assert!(Tidy(p.clone()).urge(&s).unwrap().pressure > 0.5);

        assert!(Share(p.clone()).urge(&s).is_none());
        s.self_model.people.insert("u1".into(), Acquaintance { name: "zhaoxi".into(), open_threads: vec!["看到了流星".into()], ..Default::default() });
        assert!(Share(p).urge(&s).unwrap().reason.contains("流星"));
    }

    #[test]
    fn satisfaction() {
        let p = DriveParams::default();
        let said = Outcome { said_to: vec!["u1".into()], ..Default::default() };
        assert!(Missing(p.clone()).satisfied(&said));
        assert!(!Curiosity(p.clone()).satisfied(&said), "just chatting isn't exploring");
        let explored = Outcome { tools_used: vec!["read_file".into()], ..Default::default() };
        assert!(Curiosity(p.clone()).satisfied(&explored));
        assert!(Tidy(p).satisfied(&Outcome { tools_used: vec!["journal".into()], ..Default::default() }));
    }
}
