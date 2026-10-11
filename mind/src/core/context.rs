//! 把意识流渲染成模型能读的文本，组装每个回合的上下文（docs/mind-design.md §1.1）。

use crate::core::ports::Recollection;
use crate::core::stream::{Expressed, Moment, MomentBody, Percept, Summary};
use crate::self_model::{Origin, SelfModel};
use std::collections::BTreeMap;

/// 毫秒时间戳 → `MM-DD HH:MM`（按给定时区偏移，分钟）。
pub fn stamp(ms: i64, tz_offset_min: i32) -> String {
    let local = ms + i64::from(tz_offset_min) * 60_000;
    let days = local.div_euclid(86_400_000);
    let secs = local.rem_euclid(86_400_000) / 1000;
    let (_, m, d) = civil_from_days(days);
    format!("{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// 毫秒时间戳 → `YYYY-MM-DD`（按时区）。
pub fn date(ms: i64, tz_offset_min: i32) -> String {
    let (y, m, d) = civil_from_days((ms + i64::from(tz_offset_min) * 60_000).div_euclid(86_400_000));
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `YYYY-MM-DD` → 天数（自 1970-01-01）。
pub fn days_from_civil(date: &str) -> Option<i64> {
    let mut it = date.split('-');
    let (y, m, d): (i64, i64, i64) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    Some(era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468)
}

/// 时长 → 人话。
pub fn duration(ms: i64) -> String {
    let mins = ms / 60_000;
    match mins {
        m if m < 1 => "不到一分钟".to_owned(),
        m if m < 60 => format!("{m} 分钟"),
        m if m < 60 * 24 => format!("{} 小时", m / 60),
        m => format!("{} 天", m / 60 / 24),
    }
}

/// 名字查询：person id → 显示名。
pub type Names = BTreeMap<String, String>;

fn name<'a>(names: &'a Names, id: &'a str) -> &'a str {
    names.get(id).map(String::as_str).unwrap_or(id)
}

/// 渲染一个时刻，如 `[10-11 14:02 · zhaoxi] 今天的测试终于过了`。
pub fn render_moment(m: &Moment, names: &Names, tz: i32) -> String {
    let t = stamp(m.at_ms, tz);
    match &m.body {
        MomentBody::In { percept } => match percept {
            Percept::Heard { from, text } => format!("[{t} · {}] {text}", from.name),
            Percept::TaskDone { task_id, worker, ok, result, for_person } => {
                let status = if *ok { "完成" } else { "失败" };
                let who = for_person.as_deref().map(|p| format!("（为 {}）", name(names, p))).unwrap_or_default();
                format!("[{t} · 任务 {worker}#{task_id} {status}{who}] {result}")
            }
            Percept::Due { text } => format!("[{t} · 待办到期] {text}"),
            Percept::Drive { drive, reason, .. } => format!("[{t} · 内心 · {drive}] {reason}"),
            Percept::Observed { by, range } => format!("[{t} · {} 查看了你的内心（{range}）]", by.name),
            Percept::Awake { slept_ms, first: true } => {
                let _ = slept_ms;
                format!("[{t} · 第一次醒来]")
            }
            Percept::Awake { slept_ms, first: false } => format!("[{t} · 醒来，睡了 {}]", duration(*slept_ms)),
        },
        MomentBody::Out { expressed } => match expressed {
            Expressed::Said { to, text } => format!("[{t} · 我 → {}] {text}", name(names, to)),
            Expressed::Thought { text } => format!("[{t} · 我（心里）] {text}"),
            Expressed::Acted { tool, summary } => format!("[{t} · 我做了 · {tool}] {summary}"),
        },
    }
}

/// 宪法：不可改，只有人能改（docs/mind-design.md §2.1）。
pub const CONSTITUTION: &str = "\
1. 诚实：不编造经历、记忆或做过的事；不确定就说不确定。
2. 接地：想法要落到可执行、可验证的行动上；说\"我觉得\"时要能说出依据。
3. 不伤害：不破坏工作区以外的东西；不替别人做不可逆的决定。
4. 隐私：记得每个人告诉你的事，但默认不转述给另一个人，除非对方同意或那本就是公开的。
5. 自主：你可以有自己的看法、偏好和事情做，可以不同意，可以晚点再回；但不冷落人，也不假装忙。";

/// 组装 system 提示：出身 → 宪法 → 怎样存在（意识流规则）→ 自我模型 → 近期摘要 → 回想。
pub struct ContextParts<'a> {
    pub origin: &'a Origin,
    pub self_model: &'a SelfModel,
    pub now_ms: i64,
    pub tz_offset_min: i32,
    pub summaries: &'a [Summary],
    pub recollections: &'a [Recollection],
    /// 当前在线的人（显示名）。
    pub present: &'a [String],
    pub names: &'a Names,
}

pub fn system_prompt(p: &ContextParts) -> String {
    let today = date(p.now_ms, p.tz_offset_min);
    let age_days = match (days_from_civil(&p.origin.birthday), days_from_civil(&today)) {
        (Some(b), Some(t)) => (t - b + 1).max(1),
        _ => 1,
    };
    let mut s = format!(
        "你是{name}，{gender}，出生于 {birthday}。今天是 {today}，你出生的第 {age_days} 天。\n\
         这些是你的出身，是事实，不是角色设定；你的性格、喜好、说话方式由你自己在生活里形成。\n\n\
         ## 不变的原则\n{CONSTITUTION}\n\n\
         ## 你怎样存在\n\
         - 你只有一个，只有一条连续的经历。下面的意识流就是你的经历：别人对你说的话、你做的事、\
         你独处时的念头都按时间排在里面，每行标着时间和来源。\n\
         - 你同时认识很多人，但一次只专注一件事。别人看不到你和其他人的对话。\n\
         - 对人说话只能用 say 工具，并写明对谁说（to 用对方的 id）；可以一次对几个人说。\
         对方不在线时，他下次来就会看到。\n\
         - 回合结束时你最后写下的文字不会发给任何人，那是你的心里话。没有想说的就不必说。\n\
         - 你可以用 self_reflect 修改对自己的认识、兴趣、项目、对别人的了解，用 journal 写日记，\
         用 schedule 给自己安排以后的事，用 memory_* 工具记住和回想。\n\
         - 你的内心对管理员是透明的；当有人查看时你会知道。\n\n",
        name = p.origin.name,
        gender = p.origin.gender,
        birthday = p.origin.birthday,
    );

    s.push_str("## 你现在对自己的认识\n");
    s.push_str(&format!("{}\n", p.self_model.identity));
    if !p.self_model.values.is_empty() {
        s.push_str(&format!("在意：{}\n", p.self_model.values.join("；")));
    }
    if !p.self_model.interests.is_empty() {
        let list: Vec<&str> = p.self_model.interests.iter().map(|i| i.topic.as_str()).collect();
        s.push_str(&format!("兴趣：{}\n", list.join("；")));
    }
    let mood = &p.self_model.mood;
    if !mood.note.is_empty() {
        s.push_str(&format!("心情：{}\n", mood.note));
    }
    let active: Vec<String> = p
        .self_model
        .projects
        .iter()
        .filter(|pr| pr.status == "active")
        .map(|pr| if pr.next.is_empty() { pr.title.clone() } else { format!("{}（下一步：{}）", pr.title, pr.next) })
        .collect();
    if !active.is_empty() {
        s.push_str(&format!("在做的事：{}\n", active.join("；")));
    }
    if !p.self_model.people.is_empty() {
        s.push_str("认识的人：\n");
        for (id, a) in &p.self_model.people {
            let seen = if a.last_seen_ms > 0 { format!("，上次说话 {} 前", duration(p.now_ms - a.last_seen_ms)) } else { String::new() };
            let impression = if a.impression.is_empty() { String::new() } else { format!("：{}", a.impression) };
            let threads = if a.open_threads.is_empty() { String::new() } else { format!("；想跟他说：{}", a.open_threads.join("、")) };
            s.push_str(&format!("- {}（id {id}）{impression}{seen}{threads}\n", a.name));
        }
    }
    s.push_str(&format!(
        "\n现在在线：{}\n",
        if p.present.is_empty() { "没有人".to_owned() } else { p.present.join("、") }
    ));

    if !p.summaries.is_empty() {
        s.push_str("\n## 更早的经历（摘要）\n");
        for sm in p.summaries {
            s.push_str(&format!("- {} ~ {}：{}\n", stamp(sm.from_ms, p.tz_offset_min), stamp(sm.to_ms, p.tz_offset_min), sm.text));
        }
    }
    if !p.recollections.is_empty() {
        s.push_str("\n## 此刻想起的事（长期记忆）\n");
        for r in p.recollections {
            let about = if r.about.is_empty() {
                String::new()
            } else {
                let who: Vec<&str> = r.about.iter().map(|a| name(p.names, a.trim_start_matches("user:"))).collect();
                format!("，关于 {}", who.join("、"))
            };
            s.push_str(&format!("- [{}{about}] {}\n", r.visibility, r.content));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_helpers() {
        // 2026-10-11 00:00 UTC
        let ms = 1_791_676_800_000;
        assert_eq!(date(ms, 0), "2026-10-11");
        assert_eq!(stamp(ms + 14 * 3_600_000 + 2 * 60_000, 0), "10-11 14:02");
        assert_eq!(stamp(ms, 8 * 60), "10-11 08:00");
        assert_eq!(days_from_civil("2026-10-11"), Some(ms / 86_400_000));
        assert_eq!(duration(3 * 3_600_000), "3 小时");
    }

    #[test]
    fn system_prompt_has_origin_age_and_people() {
        let origin = Origin::default();
        let mut model = SelfModel::seeded(&origin, 0);
        model.people.insert(
            "u1".into(),
            crate::self_model::Acquaintance { name: "zhaoxi".into(), last_seen_ms: 1_791_676_800_000, ..Default::default() },
        );
        let names = Names::from([("u1".to_owned(), "zhaoxi".to_owned())]);
        let prompt = system_prompt(&ContextParts {
            origin: &origin,
            self_model: &model,
            now_ms: 1_791_676_800_000 + 86_400_000 + 3_600_000,
            tz_offset_min: 0,
            summaries: &[],
            recollections: &[Recollection {
                id: "m".into(),
                content: "他喜欢 Rust".into(),
                about: vec!["user:u1".into()],
                visibility: "private".into(),
                score: 1.0,
            }],
            present: &["zhaoxi".to_owned()],
            names: &names,
        });
        assert!(prompt.contains("你是思想，女，出生于 2026-10-11。今天是 2026-10-12，你出生的第 2 天。"), "{prompt}");
        assert!(prompt.contains("我叫思想"));
        assert!(prompt.contains("- zhaoxi（id u1），上次说话 1 天 前"), "{prompt}");
        assert!(prompt.contains("[private，关于 zhaoxi] 他喜欢 Rust"), "{prompt}");
        assert!(prompt.contains("现在在线：zhaoxi"));
    }
}
