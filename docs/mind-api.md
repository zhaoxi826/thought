# 心智前端接口（给前端协作者）

> 后端已实现，见 `server/src/routes/mind.rs`。设计背景见 `docs/mind-design.md`。
> 约定沿用现有 API：错误形状 `{"error": "…"}`；除登录外都要 `Authorization: Bearer <token>`；时间戳为 Unix 毫秒。
> 启用条件：`thought.toml` 有 `[mind]` 段。未启用时 `/api/mind/*`（除 budget / pause / resume）返回 `404 {"error":"mind is not enabled"}`。

## 0. 心智改变了什么

- **没有会话了**。她（思想）只有一条意识流，所有人和她的话都在里面。前端的对话页不再有会话列表，只有"和她说话"。
- 每个人只看到**自己和她之间**的话。她独处时的念头、她和别人的对话，普通用户看不到。
- admin 经**二次鉴权**后可以看到她完整的内心（意识流、日记、自我与成长、审计）。她会知道有人看了。
- 首次对话时请提示：「你和思想的对话对管理员可见」（接口里有 `notice` 字段）。

## 1. 数据：意识流时刻 `Moment`

```json
{ "seq": 42, "at_ms": 1791700000000, "dir": "in",  "percept":   { "kind": "heard", "from": { "id": "u1", "name": "zhaoxi" }, "text": "你好" } }
{ "seq": 43, "at_ms": 1791700003000, "dir": "out", "expressed": { "kind": "said",  "to": "u1", "text": "你好，我是思想。" } }
```

`seq` 是全局唯一且单调递增的顺序，用它去重、排序、分页。

| `dir` | `kind` | 字段 | 含义 |
| --- | --- | --- | --- |
| in | `heard` | `from{id,name}`, `text` | 有人对她说话 |
| in | `task_done` | `task_id`, `worker`, `ok`, `result`, `for_person` | 她派出的任务回来了 |
| in | `due` | `text` | 她给自己安排的事到期 |
| in | `drive` | `drive`, `pressure`, `reason` | 某个驱动让她醒来（`drive`: missing / share / curiosity / project / tidy） |
| in | `observed` | `by{id,name}`, `range` | 有人查看了她的内心 |
| in | `awake` | `slept_ms`, `first` | 进程启动后醒来；`first=true` 是她第一次醒来 |
| out | `said` | `to`, `text` | 她对某人说话 |
| out | `thought` | `text` | 她的心里话（不发给任何人） |
| out | `acted` | `tool`, `summary` | 她做了一件事（预留） |

普通用户的对话视图里只会出现 `heard`（他自己说的）和 `said`（她对他说的）。

## 2. 对话页

### `POST /api/mind/say`

```json
{ "text": "今天的测试终于过了" }
```

→ `202`，返回写入意识流的那个 `Moment`（就是你自己这句话）。她的回应通过事件流到达。`400`：空消息，或超过 64K 字符。

### `GET /api/mind/conversation?after=<seq>&limit=<n>`

→ `200`：

```json
{ "moments": [ Moment, ... ], "notice": "你和思想的对话对管理员可见" }
```

- 按 `seq` 升序返回 `seq > after` 的部分，`limit` 默认 100、最大 500。
- 内容包括你说的和她对你说的，也包括你不在线时她说的。
- 打开页面时先拉一次历史；断线重连后用最后一个 `seq` 作为 `after` 补齐。

### `GET /api/mind/conversation/events`（SSE）

和现有 SSE 一样，用 fetch 流式读取，在请求头带 `Authorization`。**只要连接开着，你就算"在线"**：她会知道你在，可能会直接开口；她在你在线期间对你说的话也会实时推过来。

| event | data | 说明 |
| --- | --- | --- |
| `moment` | `Moment` | 你在别的标签页说的话（回显），或她对你说的话 |
| `thinking` | `true` / `false` | 她开始想 / 想完了（类似"对方正在输入"）。内容不可见；她在想别人的事时你也会看到 |
| `lagged` | 数字 | 你落后了，丢了事件；用 `conversation?after=` 补齐 |

她不一定每句话都回，也可能晚点再回。她有自己的事要忙，这是设计如此。

## 3. 她的状态（所有登录用户）

### `GET /api/mind/status`

```json
{
  "thinking": false,
  "identity": "我叫思想，今天是我出生的第一天。……",
  "mood": { "valence": 0.2, "arousal": 0.4, "note": "有点期待" },
  "interests": ["星星"],
  "projects": ["读完一本书"],
  "budget": { "day": "2026-10-11", "by_origin": { "user": {…}, "mind": {…} }, "fatigue": 0.1, "paused": false, … },
  "tasks": [ { "task_id": 3, "worker": "coder", "task": "…" } ],
  "heart": {
    "threshold": 0.6,
    "readings": [ { "name": "missing", "kind": "relation", "pressure": 0.42, "reason": "已经 20 小时没和zhaoxi说话了，有点想他。" }, … ],
    "last_fired": "curiosity",
    "last_fired_ms": 1791700000000
  }
}
```

可以用来做"心"页：压力条（`pressure` 对比 `threshold`）、心情、兴趣、在做的事、疲劳度。`reason` 是驱动给出的理由，**属于她的内心**。普通用户能不能看到这一项，请前端按产品意图决定；后端目前不做区分。

## 4. 内心（admin + 二次鉴权）

### `POST /api/auth/elevate`

```json
{ "password": "<admin 的密码>" }
```

→ `200 { "inner_token": "…", "expires_at": 1791700600 }`，有效期 10 分钟。

- `401`：密码错误。
- `429`：连续失败 5 次，锁定 15 分钟。
- `403`：不是 admin。

以下接口除了 `Authorization`，还要带 `X-Inner-Token: <inner_token>`。缺少或过期返回 `403`，前端应提示重新输入密码。**每次查看都会写进审计，她的意识流里也会出现一条 `observed`。**

| 方法 | 路径 | 返回 |
| --- | --- | --- |
| GET | `/api/mind/stream?before=<seq>&limit=` | 完整意识流，按 `seq` **降序**分页（往上翻） |
| GET | `/api/mind/stream/events` | SSE，`event: mind`，data 是下面的 `MindEvent` |
| GET | `/api/mind/journal?before=<at_ms>&limit=` | 日记 `[{ at_ms, text }]`，降序 |
| GET | `/api/mind/self` | `{ self_model, history: [{ at_ms, field, before, after, reason }] }`：她的自我模型和成长轨迹 |
| GET | `/api/mind/audit` | 审计：`kind=disclosure`（她对 `to` 说话时想起过别人的私事，只记录不拦截）、`kind=inner_view`（谁、何时看了什么） |

`MindEvent`（内心实时流）：

```json
{ "type": "moment", "moment": Moment }
{ "type": "thinking", "active": true }
{ "type": "turn_failed", "error": "…", "backoff_secs": 60 }
{ "type": "self_changed" }
```

## 5. 运维（admin）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/mind/budget` | 今日用量（`by_origin.user` 是人找她，`by_origin.mind` 是她自主）、软/硬上限、疲劳度、是否急停 |
| GET | `/api/mind/budget/history?days=` | 按日用量 |
| POST | `/api/mind/pause` · `/resume` | 急停 / 恢复她的自主活动。人和她说话不受影响 |

## 6. 和旧接口的关系

- 旧的 `/api/sessions/*` 还在，没有删，但启用心智后它们走的是旧的"每会话主控"，**不是她**。新前端请改用 `/api/mind/*`。
- 任务页（`/api/tasks*`）照常可用。她派出的任务以 `session_id = "mind"` 出现，归属第一个 admin。
