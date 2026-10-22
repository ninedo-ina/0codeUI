# 输出混淆与防逆向（REQ-202610-1184）

> 模块：M1 动态 UI 生成 · 输出安全层
> 上游：REQ-202610-1172 静态模板引擎、REQ-202610-1185 动态生成管线
> 下游依赖：REQ-202610-1173 单文件 HTML 校验器

## 1. 目标

1. 对下发的单文件 HTML 做高强度随机化混淆，提高逆向、抓取与静态指纹成本；
2. DOM 结构：包装节点、属性顺序打乱、诱饵节点；
3. CSS：类名按**会话级映射表**随机化，选择器同步改写；
4. JS：局部变量、函数名、参数名随机化，保留全局对象、属性访问与字符串语义；
5. 混淆后仍通过 05 校验器，且页面文本、结构、交互行为与混淆前等价；
6. 同一页面多次请求输出不同，防止静态指纹；同一会话内类名映射稳定。

## 2. 代码结构

| 路径 | 说明 |
| --- | --- |
| `server/src/obfuscation/engine.rs` | 编排：解析 → 类名映射 → CSS/JS 改写 → DOM 随机化 → 复检 |
| `server/src/obfuscation/dom.rs` | 复用校验器 tokenizer 的轻量 DOM 树与序列化 |
| `server/src/obfuscation/css.rs` | CSS 类选择器扫描 / 改写；跳过注释、字符串、`url()`、小数 |
| `server/src/obfuscation/js.rs` | JS 词法分析、声明识别、标识符重命名、字符串内类选择器改写 |
| `server/src/obfuscation/session.rs` | 会话级类名映射表（TTL + LRU 容量上限） |
| `server/src/obfuscation/rng.rs` | 无额外依赖的 xorshift64* 随机源 |
| `server/src/routes/ui_obfuscate.rs` | `POST /api/v1/ui/obfuscate` 直连接口 |
| `server/tests/output_obfuscation.rs` | 验收：校验、内容等价、随机性、会话稳定、HTTP 契约 |

## 3. 混淆强度

| 级别 | 类名 | 属性顺序 | JS 变量 | 包装节点 | 诱饵节点 |
| --- | --- | --- | --- | --- | --- |
| `off` | — | — | — | — | — |
| `basic` | ✓ | ✓ | — | — | — |
| `standard`（默认） | ✓ | ✓ | ✓ | ✓ | — |
| `aggressive` | ✓ | ✓ | ✓ | ✓（更深） | ✓ |

环境变量：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `UI_OBFUSCATION_ENABLED` | `true` | 总开关；关闭时 `/ui/render` 与 `/ui/generate` 原样返回 |
| `UI_OBFUSCATION_LEVEL` | `standard` | `off` / `basic` / `standard` / `aggressive` |
| `UI_OBFUSCATION_SESSION_TTL_SECS` | `1800` | 会话映射表过期时间 |
| `UI_OBFUSCATION_MAX_SESSIONS` | `1000` | 映射表最大会话数（超出淘汰最久未用） |
| `UI_OBFUSCATION_MIN_IDENTIFIER_COVERAGE` | `0.8` | 非保留标识符覆盖率下限；低于下限返回 500 |

请求级 `POST /api/v1/ui/obfuscate` 可覆盖 `level` 与 `min_identifier_coverage`；服务端
`off` 是总开关，请求无法重新打开。

## 4. 接口

### `POST /api/v1/ui/obfuscate`

鉴权与业务接口一致。请求：

```json
{
  "html": "<!DOCTYPE html><html>...</html>",
  "session_id": "sess_abc123",
  "obfuscation": { "level": "aggressive" }
}
```

响应：

```json
{
  "code": 0,
  "msg": "ok",
  "data": {
    "html": "<!DOCTYPE html><html>...",
    "stats": {
      "level": "aggressive",
      "session_id": "sess_abc123",
      "classes_total": 33,
      "classes_renamed": 33,
      "identifiers_total": 20,
      "identifiers_renamed": 20,
      "identifier_coverage": 1.0,
      "wrappers_added": 8,
      "decoys_added": 2,
      "attributes_shuffled": 24,
      "bytes_before": 15188,
      "bytes_after": 20123,
      "duration_ms": 2.314
    }
  }
}
```

`/api/v1/ui/render` 额外接受可选字段 `session_id` 与 `obfuscation`；`/api/v1/ui/generate`
使用服务端全局配置，响应字段保持原有 3 个字段不变。

## 5. 算法与安全边界

### 5.1 DOM

- 使用 REQ-202610-1173 的容错 tokenizer 构建轻量 DOM，不引入 HTML 解析库；
- 属性顺序随机打乱；`class` 值按映射表逐 token 替换；
- 对 `body` / `main` / `section` / `div` 等允许流内容的容器，在子元素 ≥ 2 时插入
  `<div style="display:contents">` 包装节点；`head`、`table`、`ul`、`p`、`button` 等
  内容模型敏感元素不包装；
- CSS 出现 `>`、`+`、`~`、`:nth-*`、`:first-child`、`:last-child`、`:only-child`、
  `:empty`、`:has(` 时自动关闭包装节点与诱饵节点，避免破坏布局；
- `aggressive` 在 `body` 末尾追加 `display:none` + `aria-hidden="true"` 诱饵节点。

### 5.2 CSS

- 只改写选择器中的 `.class`；跳过注释、字符串、`url(...)` 与 `.5em` 之类数字；
- 类名映射表按 `session_id` 复用；无会话请求每次生成新映射；
- 同一会话中新出现的类名增量补齐，已有映射不变化。

### 5.3 JavaScript

- 轻量词法分析识别字符串、模板字符串、注释、正则、数字与标识符；
- 只重命名**声明过的** `var` / `let` / `const`、函数名、函数参数、箭头函数参数、
  `catch` 参数；未声明全局变量一律不动；
- 跳过 `.` / `?.` 后的属性、对象字面量键、对象方法名、`#` 私有名与保留全局对象；
- 对象简写 `{el}` 自动展开为 `{el: v123}`，保持键名不变；
- 字符串中的 `.class` 选择器同步改写；`classList` 等运行时 API 不在本层猜测。

### 5.4 复检

输出必须再次通过 `validate_single_file_html_with_config`。任何结构、单文件约束或安全
基线错误都会使本次混淆失败（HTTP 500），不会把未校验的 HTML 下发。

## 6. 功能等价性

验收测试 `server/tests/output_obfuscation.rs` 覆盖：

- 混淆前后均通过校验器；
- 订单号、商品名、金额、按钮文案等可见内容不变；
- 内联 `fetch` URL、`IntimeUI.token()`、`document.getElementById`、`localStorage`、
  `window.IntimeUI` 等关键语义不变；
- 原始类名不再出现在 `class` 属性中，CSS 选择器同步改名；
- 同一页面两次请求 HTML 不同；同会话类名集合一致；不同会话类名不同；
- `identifier_coverage >= 0.8`，包装节点 / 诱饵节点 / 属性打乱计数大于 0。

单元测试还比较混淆前后的文本内容与元素标签序列（忽略包装 / 诱饵节点），确保 DOM
结构等价。

## 7. 已知边界

- JS 混淆是保守的局部标识符策略，不处理跨多个 `<script>` 的全局变量共享；模板管线当前
  为单脚本页面；
- 不解析 `eval` / `new Function` 动态代码；
- `classList.add('foo')` 这类运行时类名 API 不会被自动改写；模板应使用
  `querySelector('.foo')` 或静态 `class` 属性；
- 包装节点依赖 `display:contents`；目标 WebView 需支持该 CSS 值（Android 7+ / iOS 11+）。
