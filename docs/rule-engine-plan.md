# Snatch 规则引擎演进计划书

> 目标:在保持轻量与可维护的前提下,把当前"只能取属性"的选择器,逐步演进为
> 一套对标 Legado(开源阅读)能力、但**用结构化 TOML 表达**的抓取规则系统。

## 1. 现状与问题

当前 `ScraperRule.selectors` 只支持 `{ expression = "CSS", attribute = "属性名" }`,
通过 `el.attr(name)` 取**属性值**。缺口:

- 无法取**元素文本**(`@text`)、**内部 HTML**(`@html`)、**仅直接文本**(`@ownText`);
- 无法做**正则净化**(去水印、抽数字);
- 无法应对 **JS 动态加载**(数据在 `/api` JSON 里)或需要 **JS 后处理/解密**的站点;
- 无 XPath,部分结构靠 CSS 难表达。

`mode` 目前为 `image | text | links`,各自硬编码了取值方式,不统一。

## 2. 设计原则

1. **采纳 Legado 的"能力语义",不照搬它的字符串微语法。** Legado 的
   `class.x.0@tag.a@text##re##rep` 很强但解析脆弱;我们用结构化 TOML 字段表达同样语义,
   可读、无自定义解析器、对 IDE 友好。
2. **向后兼容。** 现有规则(`attribute = "src"` 等)行为不变;新能力全部是**可选新增字段**。
3. **CSS 为主干,XPath/JSON 为旁路。** 90% 站点 CSS + 取值 + 正则即可;XPath/JS 按需启用。
4. **依赖可控、按需引入。** 纯 Rust 优先;引入 C 依赖前先确认收益(QuickJS 复用已有 C 工具链)。
5. **抽取管线化。** `定位 → 取值 → 正则 → JS` 四段可插拔,每个引擎只负责"定位+取值"。
6. **需求驱动排期。** 每阶段都有立即可用产出;重(XPath/JS)的能力留接口,真碰到目标站再上。

## 3. 统一规则模型(目标形态)

每个选择器升级为一个可选字段更丰富的结构(均可选,缺省即现有行为):

```toml
[[rules.selectors]]
engine     = "css"        # css(默认) | xpath | json
expression = "#content"   # 选择器主体(按 engine 解释)
field      = "html"       # text | ownText | html | outerHtml | <属性名>;默认沿用 attribute
regex      = "\\s+"        # 可选:对每个取到的值做正则替换
replace    = " "          # 可选:替换目标(默认空串=删除)
js         = "result.trim()"  # 可选:QuickJS 后处理,变量 result/baseUrl
```

向后兼容:保留 `attribute`;`field` 不填时等价于 `attribute`。`attribute`/`field`
新增保留字 `text/ownText/html/outerHtml`,其余按属性名处理。

规则级新增:

```toml
[[rules]]
source = "html"   # html(默认) | json —— 决定如何抓取与解析整页
```

`source = "json"` 时:请求按 JSON 处理,选择器 `engine` 用 `json`(JSONPath)。

抽取管线(每个选择器):

```
定位(css/xpath/json) → 取值(field) → 正则(regex/replace) → JS(js) → 结果
```

## 4. 优先级路线图

按 **(价值 × 覆盖率) ÷ 成本** 排序。XPath 与 QuickJS 已确认要做,排在补齐核心之后。

| 优先级 | 能力 | 依赖 | 成本 | 说明 |
|--------|------|------|------|------|
| **P0** | 取值字段 `@text/@html/@ownText/@outerHtml/@attr` | 无 | 低 | 补最大的洞,统一三种 mode 的取值 |
| **P0** | 每选择器正则净化 `regex`/`replace` | 无(已有 regex) | 低 | 去水印、抽字段 |
| **P1** | 组合符 `\|\|`(取首个非空)/`&&`(合并)/`-`(反序) | 无 | 中 | 多选择器回退/合并,轻量解析 |
| **P2** | `source = "json"` + JSONPath | `serde_json_path`(纯 Rust,小) | 中 | 抓 JS 动态加载背后的 `/api` JSON(如 combot) |
| **P3** | XPath 引擎 `engine = "xpath"` | `skyscraper`(纯 Rust HTML XPath)优先 | 中-高 | CSS 难表达的结构;避免 C 依赖 |
| **P4** | JavaScript 后处理 `js` / `<js>` | `rquickjs`(QuickJS) | 中 | 拼 URL、解密、混淆数据;复用已有 C 工具链 |

> 排序理由:P0/P1 零依赖、覆盖最广,先落地;P2(JSONPath)纯 Rust 且能解锁一整类
> "前端渲染"站点,性价比高于 XPath;XPath(P3)与 JS(P4)最重,放后面,但**确定要做**,
> 架构在 P0 阶段就预留好引擎抽象与管线钩子。

## 5. 各阶段详细设计

### P0 — 取值字段 + 正则净化
- `SelectorDef` 增加可选 `field`、`regex`、`replace`(serde default)。
- 抽取时按 `field` 取值:
  - `text` → `el.text().collect::<String>()`
  - `ownText` → 仅直接子文本节点拼接
  - `html` → `el.inner_html()`
  - `outerHtml` → `el.html()`
  - 其它/未填 → `el.attr(field 或 attribute)`(现有行为)
- 取到值后,若有 `regex`,用 `Regex::new(regex).replace_all(value, replace)`。
- 影响面:`crawler::extract_images_impl` / links 抽取 / text 模式统一走同一取值函数 `extract_value(el, field)`。
- 收益:`{ expression = "#content", field = "html" }` 直接拿到内部 HTML;`text` 模式可由通用选择器替代。

### P1 — 组合符
- 在选择器**列表层面**实现(避免改字符串语法):
  - 现有"多选择器"语义改为可配置:`combine = "merge" | "first" | "reverse"`(规则级或选择器组级)。
  - `first`(`||`):依次尝试,取第一个非空结果。
  - `merge`(`&&`):合并所有结果(现状默认)。
  - `reverse`(`-`):结果反序(章节列表常用)。
- 轻量、无新依赖。

### P2 — JSON 源 + JSONPath
- 规则级 `source = "json"`:`crawl` 分支——把响应当 JSON,`serde_json::from_str`。
- 选择器 `engine = "json"`,`expression` 为 JSONPath(`$.data[*].title`),用 `serde_json_path` 求值。
- 适配"分页 API":复用现有 `pagination.query`(`?limit=&offset=`)。
- 解锁:combot 这类 `/api/chart/...` JSON、各种前端渲染站。

### P3 — XPath
- `engine = "xpath"`,`expression` 为 XPath(`//div[@id="content"]//a`)。
- **引擎选型**:优先 `skyscraper`(纯 Rust,直接在 HTML 上跑 XPath,**不引 C 依赖**);
  若其能力/维护不足,退回 `libxml`(libxml2 C 库,Windows 构建较麻烦)——在该阶段再评估。
- 取值字段 `field` 复用 P0 的语义(text/attr/...)。

### P4 — JavaScript(QuickJS)
- 选择器 `js`(后处理)与规则级 `<js>` 块(自由脚本)。
- **引擎**:`rquickjs`(绑定 QuickJS)。运行体小、ES2020 较完整;构建需 C 编译器——
  本项目因 wreq/BoringSSL **已强制 cmake/perl/clang**,故**不新增构建负担**。
  - 备选:`boa`(纯 Rust,免 C 工具链,但二进制更大、规范不全)。在该阶段二选一。
- 注入变量:`result`(当前值)、`baseUrl`、`page`。沙箱:无网络/文件 API,仅字符串运算。
- 安全:规则为**本地用户自撰**,非远程不可信输入;仍限制可用全局对象。
- 典型用途:拼下一页 URL、解码 base64/混淆、从脚本变量里抠数据。

## 6. 架构改动(P0 即落地的地基)

```rust
enum Engine { Css, Xpath, Json }              // 选择器引擎
fn extract_value(el, field) -> String         // 统一取值(text/html/attr...)
fn post_process(value, regex, replace, js) -> String   // 正则 + JS 管线
trait Locator { fn select(&self, root, expr) -> Vec<Node>; }  // css/xpath/json 各实现
```

- P0 先引入 `extract_value` + `regex` 后处理,把 image/text/links 三处取值收敛到一处。
- `Locator` 抽象在 P0 留好,P2/P3 各加一个实现即可,不动上层。
- JS 管线钩子(`post_process` 的 `js` 参数)在 P0 留空实现,P4 接 QuickJS。

## 7. 依赖与构建影响

| 阶段 | 新增 crate | 纯 Rust? | 构建影响 |
|------|-----------|----------|----------|
| P0/P1 | 无 | — | 无 |
| P2 | `serde_json_path` | 是 | 无 |
| P3 | `skyscraper`(优先)/ `libxml`(备选) | 是 / 否 | 纯 Rust 无影响;libxml 需 libxml2(Win 麻烦) |
| P4 | `rquickjs`(优先)/ `boa`(备选) | 否 / 是 | QuickJS 需 C 编译器(**已具备**) |

体积预估:P2 ≈ +small;P4(QuickJS)≈ +~1MB。

## 8. 兼容性

- 现有 `scraper.toml` 全部继续可用(`attribute` 保留)。
- 新字段均 `#[serde(default)]`,旧配置解析不受影响。
- `mode = image|text|links` 暂保留;P0 后 `text`/`links` 可由"通用选择器 + field"等价表达,
  未来可平滑收敛(不强制)。

## 9. 测试策略

- 每个引擎/取值字段:用内置 HTML 片段写单元测试(`#[cfg(test)]`),断言抽取结果。
- 真站点:保留一次性 `examples/probe.rs` 式探针验证选择器(用完即删)。
- 回归:固定几条代表性规则(图片/文本/links/json)的快照测试。

## 10. 里程碑

1. **M1(P0)**:取值字段 + 正则净化 + 取值管线收敛。✅ 已完成
2. **M2(P1)**:图片选择器 `combine = merge|first`(回退链)。✅ 已完成
3. **M3(P2)**:JSON 源 + JSONPath(打通 API 类站点)。✅ 已完成
4. **M4(P3)**:XPath(skyscraper),image 选择器 `engine = "xpath"`。✅ 已完成
5. **M5(P4)**:JS 后处理 `js`(改用 **Boa** 纯 Rust 引擎,免 C 工具链),Field/Column 适用。✅ 已完成

---

**下一步**:确认本计划与优先级后,从 **M1(P0)** 开工:落地 `field`(text/html/ownText/outerHtml/attr)
与 `regex/replace`,并把 image/text/links 的取值收敛到统一管线,同时铺好 `Locator`/`post_process`
抽象,为 P2–P4 预留接口。
