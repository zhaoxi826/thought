//! 心跳：真实信号 → 压力 → 点火 → 她醒来 → 满足后压力不再立即点火。

mod common;

use common::*;
use mind::*;
use std::collections::BTreeMap;
use std::sync::Arc;

fn fast() -> HeartConfig {
    HeartConfig { interval_secs: 1, threshold: 0.5, min_gap_secs: 0, fatigue_gain: 0.35, settle_secs: 0 }
}

fn heart_ports(w: &World) -> HeartPorts {
    HeartPorts {
        memory: Some(w.memory.clone()),
        metabolism: w.metabolism.clone(),
        expression: w.expression.clone(),
        clock: w.clock.clone(),
        people: Arc::new(MemPeople(people())),
    }
}

#[tokio::test(start_paused = true)]
async fn missing_someone_wakes_her_and_she_reaches_out() {
    let w = World::new(vec![
        Turn::thought(""),            // 第一次醒来
        Turn::say("u1", "好"),        // 回应 zhaoxi
        Turn::say("u1", "好久不见，在忙什么？"), // 想念点火后主动开口
        Turn::thought("想去发现点什么。"),         // 好奇也可能点火
    ]);
    let mind = w.start(MindConfig::default()).await;
    w.settle(&mind).await;
    // 和 zhaoxi 聊过很多次
    for _ in 0..1 {
        mind.perceive(heard("u1", "zhaoxi", "晚安")).await.unwrap();
        w.settle(&mind).await;
    }
    {
        // 让她和 zhaoxi 很亲近
        let model = w.selves.model.lock().unwrap().clone().unwrap();
        assert!(model.people["u1"].exchanges >= 2);
    }
    let heart = Heart::start(
        mind.clone(),
        drives::all(&DriveParams { missing_intimacy_at: 2.0, missing_tau_h: 1.0, ..Default::default() }),
        fast(),
        heart_ports(&w),
        BTreeMap::new(),
    );
    // 两天过去，他不在线
    w.clock.advance(48 * 3_600_000);
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    w.settle(&mind).await;

    let state = heart.state();
    assert!(state.fired_ms.contains_key("missing"), "{state:?}");
    {
        let seen = w.script.seen.lock().unwrap();
        let turn = seen.iter().find(|(_, _, stream)| stream.contains("内心 · missing")).expect("missing fired a turn");
        assert_eq!(turn.0, TurnKind::Autonomous);
        assert!(turn.2.contains("没和zhaoxi说话了"), "{}", turn.2);
    }
    assert_eq!(w.delivered().last().unwrap(), &("u1".to_string(), "好久不见，在忙什么？".to_string()));
    // 满足了：记下时间
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert!(heart.state().last_satisfied_ms.contains_key("missing"));
}

#[tokio::test(start_paused = true)]
async fn fatigue_raises_threshold_and_pause_blocks_firing() {
    let w = World::new(vec![Turn::thought("")]);
    let mind = w.start(MindConfig::default()).await;
    w.settle(&mind).await;
    w.memory.untidy.lock().unwrap().clone_from(&200); // 整理压力 ≈ 0.69
    *w.metabolism.0.lock().unwrap() = (1.0, true); // 很累：阈值 0.5 + 0.35 = 0.85
    let heart = Heart::start(mind.clone(), drives::all(&DriveParams::default()), fast(), heart_ports(&w), BTreeMap::new());
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let st = heart.state();
    assert!(st.threshold > 0.8, "{st:?}");
    let tidy = st.readings.iter().find(|r| r.name == "tidy").unwrap();
    assert!(tidy.pressure > 0.6 && tidy.pressure < st.threshold);
    assert!(st.last_fired.is_none(), "too tired to bother");

    // 不累了，但急停了
    *w.metabolism.0.lock().unwrap() = (0.0, false);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(heart.state().last_fired.is_none(), "paused");

    // 恢复后点火
    *w.metabolism.0.lock().unwrap() = (0.0, true);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert_eq!(heart.state().last_fired.as_deref(), Some("tidy"));
}
