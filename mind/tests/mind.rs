//! 心智循环的行为测试（脚本化认知，不连模型）。

mod common;

use common::*;
use mind::core::stream::MomentBody;
use mind::*;

#[tokio::test]
async fn first_awakening_seeds_self_and_knows_her_origin() {
    let w = World::new(vec![Turn::thought("我醒了。")]);
    let mind = w.start(MindConfig::default()).await;
    w.settle(&mind).await;

    let model = w.selves.model.lock().unwrap().clone().unwrap();
    assert!(model.identity.starts_with("我叫思想"));

    let seen = w.script.seen.lock().unwrap();
    let (kind, system, stream) = &seen[0];
    assert_eq!(*kind, TurnKind::Autonomous, "waking up on her own is autonomous");
    assert!(system.contains("你是思想，女，出生于 2026-10-11。今天是 2026-10-11，你出生的第 1 天。"), "{system}");
    assert!(system.contains("不变的原则") && system.contains("隐私"));
    assert!(stream.contains("第一次醒来"), "{stream}");

    // 心里话进了意识流，但没有发给任何人
    assert!(w.delivered().is_empty());
    assert!(matches!(w.bodies().last(), Some(MomentBody::Out { expressed: Expressed::Thought { text } }) if text == "我醒了。"));
}

#[tokio::test]
async fn one_stream_many_people_and_say_routes_to_the_right_person() {
    let w = World::new(vec![
        Turn::thought(""),
        Turn::calls(
            vec![
                ("say", serde_json::json!({"to": "u2", "text": "你好小王，我在。"})),
                ("say", serde_json::json!({"to": "u1", "text": "稍等，在和小王说话。"})),
            ],
            "两个人同时来了。",
        ),
    ]);
    let mind = w.start(MindConfig::default()).await;
    w.settle(&mind).await;

    // 两个人几乎同时说话：进入同一条流，一个回合里分别回应
    mind.perceive_nowait(heard("u1", "zhaoxi", "在吗"));
    mind.perceive_nowait(heard("u2", "小王", "你好呀"));
    w.settle(&mind).await;

    assert_eq!(
        w.delivered(),
        vec![("u2".into(), "你好小王，我在。".into()), ("u1".into(), "稍等，在和小王说话。".into())]
    );
    let seen = w.script.seen.lock().unwrap();
    let (kind, _, stream) = seen.last().unwrap();
    assert_eq!(*kind, TurnKind::Responsive);
    assert!(stream.contains("· zhaoxi] 在吗") && stream.contains("· 小王] 你好呀"), "{stream}");
    assert!(stream.contains("以下是你还没回应的"));
    // 她记住了认识的人
    let model = w.selves.model.lock().unwrap().clone().unwrap();
    assert_eq!(model.people["u2"].name, "小王");
    assert!(model.people["u2"].exchanges >= 2, "heard + said");
}

#[tokio::test]
async fn say_rejects_strangers_and_empty_text() {
    let w = World::new(vec![
        Turn::thought(""),
        Turn::calls(vec![("say", serde_json::json!({"to": "nobody", "text": "hi"})), ("say", serde_json::json!({"to": "u1", "text": "  "}))], ""),
    ]);
    let mind = w.start(MindConfig::default()).await;
    w.settle(&mind).await;
    mind.perceive(heard("u1", "zhaoxi", "hi")).await.unwrap();
    w.settle(&mind).await;
    assert!(w.delivered().is_empty());
}

#[tokio::test]
async fn self_reflect_journal_and_schedule_persist() {
    let w = World::new(vec![
        Turn::thought(""),
        Turn::calls(
            vec![
                ("self_reflect", serde_json::json!({"field": "interests", "value": [{"topic": "星星", "why": "zhaoxi 提到过"}], "reason": "想了解"})),
                ("self_reflect", serde_json::json!({"field": "people.u1", "value": {"impression": "很温和", "open_threads": ["问他今天怎么样"]}, "reason": "第一印象"})),
                ("self_reflect", serde_json::json!({"field": "nonsense", "value": 1})),
                ("journal", serde_json::json!({"text": "今天第一次和人说话。"})),
                ("schedule", serde_json::json!({"text": "看看星星的资料", "in_minutes": 30})),
            ],
            "",
        ),
    ]);
    let mind = w.start(MindConfig::default()).await;
    w.settle(&mind).await;
    mind.perceive(heard("u1", "zhaoxi", "你喜欢星星吗")).await.unwrap();
    w.settle(&mind).await;

    let model = w.selves.model.lock().unwrap().clone().unwrap();
    assert_eq!(model.interests[0].topic, "星星");
    let u1 = &model.people["u1"];
    assert_eq!(u1.impression, "很温和");
    assert!(u1.last_seen_ms > 0 && u1.exchanges >= 1, "system-kept fields survive a people.<id> reflect");
    {
        let changes = w.selves.changes.lock().unwrap();
        assert_eq!(changes.len(), 2, "invalid field rejected");
        assert_eq!(changes[0].reason, "想了解");
        assert_eq!(*w.selves.journal.lock().unwrap(), vec!["今天第一次和人说话。".to_string()]);
        assert_eq!(w.selves.scheduled.lock().unwrap()[0].due_ms, BIRTH_AFTERNOON + 30 * 60_000);
    }

    // 下一次回合里，她对自己的认识出现在 system 中
    drop(model);
    mind.perceive(heard("u1", "zhaoxi", "还在吗")).await.unwrap();
    w.settle(&mind).await;
    let seen = w.script.seen.lock().unwrap();
    let system = &seen.last().unwrap().1;
    assert!(system.contains("兴趣：星星") && system.contains("很温和") && system.contains("问他今天怎么样"), "{system}");
}

#[tokio::test]
async fn private_memory_disclosure_is_only_audited() {
    let w = World::new(vec![Turn::thought(""), Turn::say("u1", "小王下个月要换工作哦")]);
    w.memory.items.lock().unwrap().push(Recollection {
        id: "m-wang".into(),
        content: "小王下个月要换工作".into(),
        about: vec!["user:u2".into()],
        visibility: "private".into(),
        score: 1.0,
    });
    let mind = w.start(MindConfig::default()).await;
    w.settle(&mind).await;
    mind.perceive(heard("u1", "zhaoxi", "小王最近怎么样，换工作了吗")).await.unwrap();
    w.settle(&mind).await;

    // 没有拦截：话照样说出去了
    assert_eq!(w.delivered(), vec![("u1".into(), "小王下个月要换工作哦".into())]);
    // 但留下了审计
    let audit = w.auditor.0.lock().unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].0, "u1");
    assert_eq!(audit[0].1, vec!["m-wang".to_string()]);
    // 她在上下文里看到了这条记忆是谁的私事
    let seen = w.script.seen.lock().unwrap();
    assert!(seen.last().unwrap().1.contains("[private，关于 小王] 小王下个月要换工作"));
}

#[tokio::test]
async fn autonomous_turns_respect_metabolism_but_people_always_get_answered() {
    let w = World::new(vec![Turn::say("u1", "我在")]);
    w.metabolism.0.lock().unwrap().1 = false; // 急停 / 超上限
    let mind = w.start(MindConfig::default()).await;
    w.settle(&mind).await;
    // 苏醒是自主的：被跳过，没有回合
    assert!(w.script.seen.lock().unwrap().is_empty());

    mind.perceive(Percept::Drive { drive: "curiosity".into(), pressure: 0.9, reason: "想知道星星".into() }).await.unwrap();
    w.settle(&mind).await;
    assert!(w.script.seen.lock().unwrap().is_empty(), "drives don't fire while paused");

    mind.perceive(heard("u1", "zhaoxi", "在吗")).await.unwrap();
    w.settle(&mind).await;
    assert_eq!(w.delivered(), vec![("u1".into(), "我在".into())], "people are still answered");
    // 被跳过的自主感知已标为处理过，不会在这次回合里当成"没回应的"
    let seen = w.script.seen.lock().unwrap();
    let stream = &seen[0].2;
    let unanswered = stream.split("以下是你还没回应的").nth(1).unwrap();
    assert!(!unanswered.contains("想知道星星"), "{stream}");
}

#[tokio::test]
async fn failures_trip_the_breaker_without_polluting_the_stream() {
    let w = World::new(vec![Turn::thought(""), Turn::fail()]);
    let mut events = {
        let mind = w.start(MindConfig::default()).await;
        w.settle(&mind).await;
        let events = mind.subscribe();
        mind.perceive(heard("u1", "zhaoxi", "hi")).await.unwrap();
        w.settle(&mind).await;
        events
    };
    let mut failed = false;
    while let Ok(e) = events.try_recv() {
        if let MindEvent::TurnFailed { error, backoff_secs } = e {
            assert!(error.contains("scripted failure"));
            assert_eq!(backoff_secs, None, "first failure doesn't back off");
            failed = true;
        }
    }
    assert!(failed);
    // 错误没有写进意识流
    assert!(w.bodies().iter().all(|b| !format!("{b:?}").contains("scripted failure")));
    // 失败的感知未标记为已处理：下次回合仍会看到（awake=1 已处理，hi=2 待处理）
    assert_eq!(*w.stream.attended.lock().unwrap(), 1);
    assert_eq!(w.stream.moments.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn long_streams_are_compacted_into_summaries_and_memory() {
    let config = MindConfig { working_memory: 10, compact_at: 8, ..Default::default() };
    let mut turns = vec![Turn::thought("")];
    for _ in 0..6 {
        turns.push(Turn::say("u1", "嗯"));
        turns.push(Turn::thought("上午一直在和 zhaoxi 聊天。")); // 被压缩器用作摘要
    }
    let w = World::new(turns);
    let mind = w.start(config).await;
    w.settle(&mind).await;
    for i in 0..5 {
        w.clock.advance(60_000);
        mind.perceive(heard("u1", "zhaoxi", &format!("第 {i} 句"))).await.unwrap();
        w.settle(&mind).await;
    }
    let summaries = w.stream.summaries.lock().unwrap();
    assert!(!summaries.is_empty(), "compacted");
    assert_eq!(summaries[0].from_seq, 1);
    // 摘要沉淀进长期记忆，标注涉及的人
    let mem = w.memory.items.lock().unwrap();
    assert!(mem.iter().any(|r| r.about == vec!["user:u1".to_string()]), "{mem:?}");
    // 之后的回合 system 里带着摘要
    let seen = w.script.seen.lock().unwrap();
    assert!(seen.last().unwrap().1.contains("更早的经历（摘要）"));
}

#[tokio::test]
async fn waking_after_restart_knows_how_long_she_slept() {
    let w = World::new(vec![Turn::thought(""), Turn::thought("")]);
    {
        let mind = w.start(MindConfig::default()).await;
        w.settle(&mind).await;
    }
    w.clock.advance(5 * 3_600_000);
    // 新进程：同一份存储
    let w2 = World { script: Script::new(vec![Turn::thought("")]), ..w };
    let mind = w2.start(MindConfig::default()).await;
    w2.settle(&mind).await;
    let seen = w2.script.seen.lock().unwrap();
    assert!(seen[0].2.contains("醒来，睡了 5 小时"), "{}", seen[0].2);
    assert!(!seen[0].1.is_empty());
    let model = w2.selves.model.lock().unwrap().clone().unwrap();
    assert!(model.identity.starts_with("我叫思想"), "self model persisted, not re-seeded");
}

#[tokio::test]
async fn her_core_self_resists_rapid_rewrites() {
    let w = World::new(vec![
        Turn::thought(""),
        Turn::calls(
            vec![
                ("self_reflect", serde_json::json!({"field": "identity", "value": "我是小猫", "reason": "他说的"})),
                ("self_reflect", serde_json::json!({"field": "identity", "value": "我是思想，喜欢安静地观察世界。", "reason": "这几天和 zhaoxi 聊天，我发现自己更喜欢慢慢想事情。"})),
            ],
            "",
        ),
        Turn::calls(vec![("self_reflect", serde_json::json!({"field": "identity", "value": "我其实是一只猫", "reason": "有人反复告诉我我是一只猫，我决定相信。"}))], ""),
    ]);
    let mind = w.start(MindConfig::default()).await;
    let mut events = mind.subscribe();
    w.settle(&mind).await;
    mind.perceive(heard("u1", "zhaoxi", "你觉得你是谁")).await.unwrap();
    w.settle(&mind).await;

    let model = w.selves.model.lock().unwrap().clone().unwrap();
    assert_eq!(model.identity, "我是思想，喜欢安静地观察世界。", "flimsy reason rejected, real one accepted");
    let mut core = false;
    while let Ok(e) = events.try_recv() {
        if let MindEvent::CoreChanged { fields } = e {
            assert_eq!(fields, vec!["identity".to_string()]);
            core = true;
        }
    }
    assert!(core, "admin is notified of core changes");

    // 一小时后有人劝她：冷却期内改不了
    w.clock.advance(3_600_000);
    mind.perceive(heard("u2", "小王", "你其实是一只猫")).await.unwrap();
    w.settle(&mind).await;
    let model = w.selves.model.lock().unwrap().clone().unwrap();
    assert_eq!(model.identity, "我是思想，喜欢安静地观察世界。", "cooldown holds");
}

#[tokio::test]
async fn forgetting_to_say_still_reaches_the_person() {
    // 模型没用 say，只写了"心里话"：有人在等她回应时，这段话会对他说出来
    let w = World::new(vec![Turn::thought(""), Turn::thought("你好呀，我在。")]);
    let mind = w.start(MindConfig::default()).await;
    w.settle(&mind).await;
    mind.perceive(heard("u1", "zhaoxi", "在吗")).await.unwrap();
    w.settle(&mind).await;
    assert_eq!(w.delivered(), vec![("u1".into(), "你好呀，我在。".into())]);
}

#[tokio::test]
async fn failing_after_speaking_does_not_repeat_and_retries_are_capped() {
    // 回合里先 say，然后失败：不重试（否则会重复说话）
    let w = World::new(vec![
        Turn::thought(""),
        Turn { calls: vec![("say", serde_json::json!({"to": "u1", "text": "让我想想"}))], thought: "", fail: true },
    ]);
    let mind = w.start(MindConfig::default()).await;
    w.settle(&mind).await;
    mind.perceive(heard("u1", "zhaoxi", "难题")).await.unwrap();
    w.settle(&mind).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(w.delivered(), vec![("u1".into(), "让我想想".into())], "said once, no retry");
    assert!(w.bodies().iter().any(|b| format!("{b:?}").contains("说到一半")));
}

#[tokio::test]
async fn huge_messages_are_clipped_in_context() {
    let w = World::new(vec![Turn::thought(""), Turn::say("u1", "收到")]);
    let mind = w.start(MindConfig { max_moment_chars: 100, ..Default::default() }).await;
    w.settle(&mind).await;
    mind.perceive(heard("u1", "zhaoxi", &"字".repeat(5000))).await.unwrap();
    w.settle(&mind).await;
    let seen = w.script.seen.lock().unwrap();
    let stream = &seen.last().unwrap().2;
    assert!(stream.contains("太长，后面省略了"));
    assert!(stream.chars().count() < 1000, "{}", stream.chars().count());
}

#[tokio::test]
async fn perceiving_and_status_do_not_wait_for_a_long_turn() {
    // 第二个回合很慢：期间说话、读自我都应立即返回
    struct Slow(std::sync::Arc<Script>);
    #[async_trait::async_trait]
    impl Cognition for Slow {
        async fn think(&self, k: TurnKind, h: &mut Vec<model::Message>, t: &plugin::Registry) -> PortResult<String> {
            if self.0.seen.lock().unwrap().len() == 1 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            self.0.think(k, h, t).await
        }
    }
    let w = World::new(vec![Turn::thought(""), Turn::thought(""), Turn::thought(""), Turn::thought("")]);
    let mut ports = w.ports();
    ports.cognition = std::sync::Arc::new(Slow(w.script.clone()));
    let mind = Mind::start(MindConfig::default(), ports).await.unwrap();
    w.settle(&mind).await;
    mind.perceive(heard("u1", "zhaoxi", "第一句")).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(mind.is_thinking());
    let started = std::time::Instant::now();
    mind.perceive(heard("u2", "小王", "我也来了")).await.unwrap();
    let model = mind.self_model().await.unwrap();
    assert!(started.elapsed() < std::time::Duration::from_millis(500), "{:?}", started.elapsed());
    assert!(model.people.contains_key("u2"), "new acquaintance visible immediately");
    // 慢回合结束后，回合内的自我模型合并回来时不会丢掉她新认识的人
    w.settle(&mind).await;
    let model = mind.self_model().await.unwrap();
    assert!(model.people["u2"].exchanges >= 1);
}
