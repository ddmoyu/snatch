# Snatch 源规则编写指南(给 AI 看)

> 用途:用户给你**一个链接** + **想抓什么(图片 / 文本 / 表格数据)**,你据此在
> **软件同级的 `sources/` 目录**下生成一个 `*.toml` 源配置文件。
>
> **本文件是自洽的完整 schema(权威)。** 你只需要这份文档 + 那个单文件程序即可工作,
> 不需要任何源码。所有字段语义、默认值、运行/验证方式都写在本文中。

---

## ⛔ 硬性规则(MUST,必须严格遵守,不得自行变通)

**这些规则高于一切。违反任何一条都视为任务失败。**

1. **唯一允许的工具是 `snatch`。** 取 HTML、验证、下载网页内容,**只能**通过本程序命令行:`snatch fetch`(取页面)、`snatch test`(验证规则)、`snatch run`(正式抓取)。
2. **严禁使用任何其它工具去获取或验证网页**,包括但不限于:你内置的网页抓取/浏览/搜索工具、`curl`/`wget`/`Invoke-WebRequest`、
   Python `requests`/`httpx`/`urllib`/`BeautifulSoup`、Node `fetch`/`axios`、无头浏览器、任何在线代理或"读取网页"能力。**一个都不许用。**
3. **为什么必须如此:** 本程序用 **wreq + Chrome136 的 TLS 指纹**抓取。换任何别的客户端,指纹就不同 → 目标站可能返回**不同的 HTML 或直接拦截**。
   **你用别的工具看到的页面 ≠ 程序实际抓到的页面,测了也不算数。只有 `snatch test` 的输出才是事实依据。**
4. **必须先验证再交付。** 每条规则交付前,**必须**已用 `snatch test <URL> --source <你的toml>` 跑通,亲眼看到 `OK` + 正确样本。
   "我觉得选择器应该对"但没用 `snatch` 实测过的,**一律不算完成**。
5. **失败时不许换工具绕过。** 若 `snatch test` 报错(如 `[page-err] 403`、`nothing extracted`),正确做法是**改配置(`[headers]`/`delay_ms`/选择器)后再 `snatch test`**;
   **绝不**改用别的抓取工具"看看能不能拿到"——那对本程序毫无意义,也违反本规则。
6. **不臆造。** 引擎、取值键、字段只能用本文档列出的;不确定就用 `snatch test` 求证,不要编造。

> 一句话:**凡涉及"抓网页",从头到尾只用 `snatch`,别无他法。** 你自带的任何抓取/浏览工具在本任务中一律不可用、不可信、不许用。

---

## 0. 工作流(你每次要做的事,按顺序)

1. **拿到**:目标 URL + 用户想要的内容类型。
2. **判型**:决定 `type = "image" | "text" | "data"`(见 §2)。
3. **取 HTML 定选择器**:用 `snatch fetch <URL> --strip` 看页面结构,定承载内容的选择器(容器、img、正文、行/列)。详见 §"🔎"。
4. **写规则文件**:在**程序同级的 `sources/` 目录**下新建 `<英文短名>.toml`(目录不存在就创建;文件名小写+连字符,如 `myblog-novel.toml`);不要覆盖同名文件。
5. **自测(必做,只能用 `snatch`)**:`snatch test <URL> --source <你的toml>` → 看到 `OK` 且样本正确才算通过。**禁止用任何其它工具验证**(见"⛔ 硬性规则")。
6. **正式抓取(需要真的保存数据时)**:`snatch run <URL>`。数据按 §"输出位置"保存(有 `settings.toml` 用其目录,否则存到程序同级 `data/`)。
7. **保持最小**:只写必要字段,缺省即默认行为(参考 §7 真实样例)。

> 目录位置:`sources/`(规则)和 `data/`(默认数据输出)都在**程序同级目录**;开发期跑 `cargo run` 时即 `target/debug/` 下。不存在会自动创建。

---

## 🔧 程序怎么跑、怎么匹配、怎么验证(无源码也能闭环)

程序有**四种运行模式**,共用同一套抓取管线(同一 wreq + Chrome136 TLS 指纹),结果一致:

| 模式 | 怎么启动 | 行为 | 用途 |
|------|----------|------|------|
| **`fetch`** | `snatch fetch <URL>` | 打印程序抓到的**原始 HTML**(到 stdout),不抽取、不保存 | ① 看页面结构、定选择器 |
| **`test`** | `snatch test <URL>` | 按规则抓取+打印**抽取结果**后退出,**不下载不保存不写库** | ② 调规则自测 |
| **`run`** | `snatch run <URL>` | 一次性:下载+保存+写库,**完成即退出** | ③ 跑完即走的单次抓取 |
| **常驻 TUI**(默认) | 直接运行,无参数 | 剪贴板触发,一直挂着,下载并保存、写数据库 | 日常使用 |

**写一条规则的标准三步(全程只用 `snatch`):**
```
snatch fetch <URL> --strip --limit 8000   # ① 取 HTML 看结构(--strip 去掉 script/style 噪声)
   ↓ 据此写 toml 选择器
snatch test  <URL> --source 你的.toml      # ② 验证能否抽到数据,看 OK + 样本
   ↓ 通过后
snatch run   <URL>                         # ③ 正式抓取并保存(可选)
```

> `fetch`/`test`/`run` 都是**独立进程、一次性、跑完退出**,可在常驻 TUI 运行时同时调用,互不干扰。
> `fetch`/`test` 不保存任何东西;`run` 保留数据(和常驻模式一样落盘+记历史)。

### 常驻 TUI 触发方式
1. 直接运行(双击 exe / 单文件程序),不带参数。启动时**一次性**加载 `sources/` 下所有 `*.toml`。
2. 用户**复制(Ctrl+C)一个链接**到剪贴板 → 程序自动判断该 URL 命中哪个源 → 开始抓取。
3. 不需要手动选源;靠下面的"匹配规则"自动选。

### 匹配规则(决定你的源会不会被选中)— **务必写对**
一个 URL 命中某个源,需**同时**满足:
- **是 http(s) 链接**(且含 `.`);
- **域名匹配**:URL 的 host **等于** `domains` 里某项,**或**以 `.该项` 结尾。
  - 例:`domains = ["example.com"]` 命中 `example.com` 和 `www.example.com`;**不要**写成 `https://example.com` 或带路径,只写主域。
- **`match` 子串(若设置)**:URL(忽略大小写)需**包含** `match` 字符串。
- **多个源都可能命中时,取文件名字母序靠前的第一个。** 同站多栏目务必用 `match` 区分,否则会被别的源抢走。
- TOML 写错会被**跳过并记一条 `[source-err]` 日志**,不会崩溃——所以"源没生效"时先怀疑语法或匹配。

### ① 取 HTML:`fetch` 命令(写选择器前先看页面)

要写选择器,先用 `fetch` 拿到**程序自己抓到的原始 HTML**(同一 TLS 指纹,所见即抽取器所见):
```
snatch fetch <URL> [--strip] [--limit N]
```
- HTML 打到 **stdout**(状态行打到 stderr,所以可以直接重定向/捕获干净的 HTML)。
- `--strip`:去掉 `<script>`/`<style>` 块的噪声,只留结构(标签/class/id),更好读。
- `--limit N`:最多打印 N 个字符(HTML 很大时用,避免刷屏);末尾会提示被截断。
- 失败会打印 `[page-err]` 诊断(403/超时等)。
- **这就是"先获取内容"的唯一正确手段**——不要用别的工具取 HTML(见「⛔ 硬性规则」)。

### ② 自测:用程序自带的 `test` 命令(**首选,务必用它**)

程序提供一个**对外命令行接口**专门给规则作者/AI 自测:它用**程序自己的 wreq + Chrome136 TLS 指纹**真实抓取并打印抽取结果,然后退出——**不下载、不写数据库、不去重**。
**不要用你自己的 HTTP 工具去验证**:那是浏览器/通用客户端的指纹,拿到的页面可能与本程序不同(被反爬挡、返回不同 HTML),测了也不算数。**必须通过本程序的 `test` 来确认能不能抓到。**

> **完全隔离,可在主程序运行时随时调用。** 本程序不是单例:`snatch test` 是**独立进程**,与正在运行的
> TUI 互不干扰——**不写数据库、不进任务列表、不在 TUI 显示、退出即清空,不保存任何内容**。
> 输出带明显的「规则测试 / RULE TEST」横幅。所以 AI 可以放心地反复调用来调规则,不会污染用户的真实数据。

```
snatch test <URL> [--source <file.toml>] [--limit N]
```
- 无 `--source`:按真实规则(domains/match)在 `sources/` 里自动选源——**等价于实际使用场景**。
- 有 `--source <file>`:直接用指定的 toml 文件测试。**写新规则时强烈建议用它**:无需放进 `sources/`、无需重启、改一版测一版,最快。
- `--limit N`:样本条数(默认 10)。
- Windows 开发期用 `cargo run -- test <URL> --source <file>`;发布版直接 `snatch test ...`。

**输出怎么读:**
```
[test] source = Example (type=text, output=txt)   # 选中的源
... [page] 1: 1256 bytes, 1 ...                    # 每页抓到几条(M=0 → 选择器没命中)
[test] OK: 30 chars of text                        # 成功 + 数量
---- preview ----  ...正文/图片URL/数据行样本...  ---- /preview ----
```
- `OK: N image URL(s) / N row(s) / N chars` + 样本 → ✅ 抓到了,核对样本是否就是想要的内容。
- `NO SOURCE matches ...` → 域名/`match` 写错(见上"匹配规则")。
- `FAIL: nothing extracted` → 源匹配上了但选择器没命中,**改选择器再测**。
- 日志行里出现 `[page-err] HTTP 403` 等 → 被反爬挡,需加 `[headers]`/调大 `delay_ms`(此时换别的 HTTP 工具也无意义,问题在目标站对该指纹的策略)。

> **写规则的标准动作:** `snatch fetch <URL> --strip` 看 HTML → 写 toml → `snatch test <URL> --source <toml>` 看 `OK`/样本 →
> 不对就改选择器重测 → 满意后把文件放进 `sources/`(需要保存数据再 `snatch run`)。

### 🚀 一次性抓取并保存:`run` 命令(跑完即退出,数据保留)

规则验证没问题后,要**真的抓一次并把结果存下来**(不想常驻挂着 TUI),用 `run`:

```
snatch run <URL> [--source <file.toml>] [--force]
```
- 行为 = 一次完整正式抓取:**下载 + 保存到 `download_dir` + 写入历史数据库**,然后退出。和常驻 TUI 抓一次完全等价。
- 选源规则与 `test` 相同(无 `--source` 则按 domains/match 自动选)。
- **去重**:默认遵守历史库,已抓过的 URL 会 `[run] SKIP: already downloaded`;加 `--force` 强制重抓。
- 与 `test` 的唯一区别:**`run` 保留数据,`test` 不留**。
- 输出末尾打印 `[run] OK: <标题> — N items` 和 `[run] saved to: <目录>`。

> 选择哪条命令:**只想验证选择器 → `test`(不留痕);想真正抓取拿数据 → `run`(保存)。**

### (备选)在 TUI 实际场景里验证
也可放进 `sources/` → **重启程序**(源只在启动时加载,无热重载)→ 复制链接触发 → 看 TUI 的 **Logs 面板**
(按 `c` 清空)。日志标签同上(`[match]`/`[page]`/`[ok]`/`[fail]`/`[page-err]` …)。
注意 TUI 与 `run` 模式都有 **SQLite 去重**:同一 URL 第二次会跳过(`run` 用 `--force`、TUI 用重试键绕过);
`test` 没有去重限制,所以**调试优先用 `test`**。

### 输出位置(数据存到哪)
保存目录的决定规则:
- **程序同级有 `settings.toml`** → 按其 `[general] download_dir` 保存。
- **没有 `settings.toml`** → 保存到**程序同级的 `data/` 目录**(不存在自动创建)。`settings.toml` 是**可选**的,不需要就别建。
- 目录结构:每个任务一个以**标题**命名的子目录;image→图片文件、text→`.txt`、data→`.csv`。

最小的 `settings.toml`(只在想改默认保存位置时才建)只需:
```toml
[general]
download_dir = "~/Desktop/Snatch"   # 支持 ~ ;其余字段省略走默认
```

---

## 🔎 怎么定选择器(先取 HTML,只能用 `snatch fetch`)

1. **首选:`snatch fetch <URL> --strip --limit 8000`** 取到程序自己抓的 HTML,定位承载内容的元素,挑稳定选择器(优先 `id`、语义化 `class`)。
   这是唯一允许的"看页面"方式——**不要用你自己的抓取/浏览工具**(见「⛔ 硬性规则」:别的工具指纹不同,看到的 HTML 可能和程序不一致)。
2. **`fetch` 被反爬挡住时**(返回 `[page-err] 403` 等):先尝试给规则加 `[headers]`(Cookie/Referer 等),再用 `snatch test` 验证;
   仍拿不到就请用户**粘贴目标页面 HTML 片段**(浏览器右键"检查"复制相关节点),据此写选择器。
3. **实在没有页面信息**:用该类站点最常见的稳妥选择器(图片 `img`/容器 `#content`、正文 `.content`/`.entry-content`/`.post-body`),
   文件**顶部加注释 `# TODO 选择器待核对`**,并**必须**用 `snatch test` 跑一遍确认(看 `OK`/样本/`[page]` 计数)。
> 永远不要假装选择器一定对;凡未经 `snatch test` 验证的,都说明是猜测。

---

## 1. 文件骨架

```toml
name = "人类可读名称"          # 必填,可中文,用作输出目录/文件名
type = "image"                # 必填:image | text | data
domains = ["example.com"]     # 必填:只填主域;host 等于它或以 .它 结尾即命中
match = "/photos/"            # 可选:URL 路径需包含的子串,用于区分同站不同栏目
# enabled = true              # 可选,默认 true;设 false 临时停用
# format = "html"             # 可选:html(默认) | json,仅 data 类型用到 json
# output = "files"            # 可选,默认按类型:image→files、text→txt、data→csv
# delay_ms = 300              # 可选,翻页/抓详情页之间的间隔毫秒,默认 300
# [headers]                   # 可选,附加请求头,值可用 ${ENV_VAR}
# Cookie = "token=${MY_TOKEN}"

# 然后是与 type 对应的一个块:[image] / [text] / [data]
# 以及可选的 [pagination]
```

**根字段速查**

| 字段 | 必填 | 说明 |
|------|------|------|
| `name` | ✅ | 显示名;图片/文本会用页面标题做实际目录名,数据用 `name` |
| `type` | ✅ | `image` / `text` / `data` |
| `domains` | ✅ | 字符串数组,只填主域(如 `example.com`);host 等于它或以 `.它` 结尾即匹配 |
| `match` | ❌ | URL 需包含的子串(常用路径段如 `/photos/`);同站多栏目用它分流,避免被别的源抢走 |
| `enabled` | ❌ | 默认 `true` |
| `format` | ❌ | `html`(默认)/ `json`;`json` 仅对 `type="data"` 生效 |
| `output` | ❌ | 输出格式,通常省略走默认 |
| `delay_ms` | ❌ | 请求间隔,默认 300;站点严格时调大(如 1000) |
| `[headers]` | ❌ | 自定义请求头;`${VAR}` 会用环境变量替换 |

---

## 2. 怎么判 `type`

| 用户想要 | type | 产物 |
|----------|------|------|
| 图片(图集、相册、漫画图) | `image` | 下载所有图片为文件 |
| 文章 / 小说 / 正文文本 | `text` | 合成一个 `.txt` |
| 列表/表格(名称+链接+字段…) | `data` | 导出 `.csv`(多列) |

经验:**"把页面上的图片都存下来"→image;"把这篇/这部的文字存成 txt"→text;"把列表里每条的几个字段做成表"→data。**

---

## ⚡ 性能排序与选型优先级(请优先用最快的)

抓取速度由"用哪种定位/数据源"决定,差距很大。**默认按下面的顺序选,只有上一档表达不了时才降到下一档。**

### 数据源层(先决定从哪抓)
1. **🥇 JSON API(`format="json"`)** — 站点若有返回 JSON 的接口(看 Network 里的 `/api`、`.json`、XHR),
   直接打接口最快:载荷小、无 HTML DOM、`serde_json` 解析极快。**能用 API 就别爬渲染后的页面。**
2. **🥈 HTML 页面(默认)** — 没有可用接口时再爬 HTML。

### 定位引擎层(在 HTML 页面内选选择器)
1. **🥇 CSS(默认 `engine="css"`)— 最快,首选。**
   整页只解析一次 DOM,所有 CSS 选择器复用它;引擎成熟(scraper/html5ever)。**90% 的站点 CSS 足够。**
2. **🥈 XPath(`engine="xpath"`)— 明显更慢,仅在 CSS 实在表达不了时用。**
   实现上**每个 xpath 字段都会把整页 HTML 重新解析一遍**(skyscraper),字段越多越慢;且仅 image 的 Field 支持。
   能改写成 CSS 的结构一律用 CSS。

### 后处理层(取到值之后)
1. **🥇 `regex`/`replace` — 便宜,首选做净化/抽取。**
2. **🥈 `js`(Boa)— 较贵**,每个值都过一次 JS 引擎。只有正则做不到(拼接、解码、条件逻辑)时才用。

### 其它影响速度的点
- **用 `container` 缩小范围**:把选择器限定在内容容器内,减少全文档扫描。
- **选择器数量越少越好**:`images = [...]` 里每多一个选择器就多一遍扫描。
- **`combine = "first"` 比 `merge` 略快**:命中第一个就停;但要"全收"时仍用 `merge`。
- **`delay_ms` 是礼貌不是性能**:它是请求间隔(防封),只在站点严格时调大,不要为"求快"调到 0。

> **一句话默认策略:有 JSON 接口走 JSON;否则 HTML + CSS + regex;XPath/js 只当兜底。**

---

## 3. 取值管线(三种 type 通用)

每个选择器取值都走:**定位(CSS/XPath) → 取值(`get`) → 正则净化(`regex`/`replace`) → JS 后处理(`js`)**。

### `get` 取值键
| `get` 值 | 含义 |
|----------|------|
| `text` | 元素及子孙的文本(默认值) |
| `ownText` | 仅元素直接子文本节点 |
| `html` 或 `innerHtml` | 内部 HTML |
| `outerHtml` | 含自身标签的 HTML |
| `@属性名` | 取该属性,如 `@src`、`@href`、`@data-src`、`@srcset` |

**`@属性` 的特殊处理(用于 URL):** 自动忽略 `data:`/`blob:`,`@srcset` 自动挑最佳清晰度,
并把相对链接解析为**绝对 URL**。所以图片/链接一律用 `@src`/`@href` 之类,不用手动拼域名。

### `regex` / `replace`(可选,净化)
对取到的每个值做正则替换:`Regex::new(regex).replace_all(value, replace)`。
`replace` 省略即删除匹配部分。例:抽数字 `regex = "[^0-9.]"`, `replace = ""`。

### `js`(可选,后处理)
一段 JS,作用域内有 `result`(当前值,字符串)和 `baseUrl`。脚本的求值结果即新值;
**出错则原值不变**。纯字符串运算,无网络/文件 API。例:`js = "result.trim().toUpperCase()"`。
适用:Field、Column(仅 HTML 模式)、text 的正文。

### `engine`(可选,仅 Field)
默认 `css`(**首选,最快**)。`engine = "xpath"` 时 `selector` 写 XPath(如 `//div[@id='content']//img`),取值键复用 `get`。
**xpath 明显更慢**(每个 xpath 字段都会整页重新解析一次),且**仅 image 的 `images`/`detail.images`(即 Field)支持**;
data 的 column、text 的选择器不支持。**能用 CSS 表达就别用 xpath。**

---

## 4. `type = "image"`

```toml
[image]
container = "#content"        # 可选:把后续选择器限定在此容器内
images = [                    # 必填:取图选择器列表(Field)
    { selector = "img", get = "@src" },
]
# exclude = [".ad", ".thumb"] # 可选:命中这些选择器的元素排除掉
# combine = "merge"           # 可选:merge(默认,全取+去重) | first(取首个有结果的选择器)
```

**懒加载站点用 `combine = "first"` 做回退链**(真实图在 `data-src`,占位图在 `src`):
```toml
[image]
container = ".tpc_content"
combine = "first"
images = [
    { selector = "img", get = "@data-src" },
    { selector = "img", get = "@src" },
]
```
> `merge` 会把 data-src 和占位 src 都收进来(可能混入占位图);`first` 只取第一个有结果的选择器。
> 已知都是真链接、要全收时用 `merge`;有占位图要回退时用 `first`。

**两级抓取(列表页 → 详情页)**:列表页只有详情链接,真图在详情页里。
```toml
[image]
container = ".list"
images = []                  # 列表页本身不取图

[image.detail]
link = "a.item"              # 详情页链接选择器(取其 href)
container = "#gallery"       # 详情页内的图片容器
images = [
    { selector = "img", get = "@src" },
]
# combine / exclude 同样可用
```

---

## 5. `type = "text"`

### 5a. 单篇正文(最常见)
```toml
[text]
content = ".post-body"       # 正文容器选择器
get = "text"                 # 可选,默认 text;要保留 HTML 用 html
# title = "h1.title"         # 可选;不填则用 <title>
# author = ".author"         # 可选
# date = ".date"             # 可选
# convert = "simplify"       # 可选:繁→简
# strip = ["广告词", "book18.org"]  # 可选:从正文删除这些字符串
# js = "result.replace(/\\s+/g,' ')"  # 可选:对每段正文做 JS 后处理
```
> 预处理:`</p><p>`→空行、`<br>`→换行,并自动剔除 `<style>`/`<script>`。

### 5b. 一页多段(如博客一页多篇、合集)
```toml
[text.sections]
each = ".article"            # 每个区块的选择器
content = ".body"            # 区块内正文(相对 each)
# title = "h2"               # 可选,区块标题(相对)
# date = ".time"             # 可选(相对)
# get = "text"               # 可选,默认 text
```
区块之间用分隔线连接。

### 5c. 目录页 → 逐章抓取(小说整本)
```toml
[text.chapters]
links = "#chapterlist a"     # 目录里每章链接(取 href)
content = "#content"         # 章节页正文选择器
# title = "h1"               # 可选,章节标题
# get = "text"               # 可选
```
> 有 `[text.chapters]` 时,入口 URL 当作目录页;会依次抓每个链接的正文,合成整本。
> `convert`/`strip`/`js` 仍在 `[text]` 顶层设置,对全本生效。

---

## 6. `type = "data"`(列表 → CSV)

### 6a. HTML 表格/列表
```toml
[data]
# container = "table.list"   # 可选:限定范围
row = "tr.item"              # 必填:每一"行"的选择器
[[data.columns]]
name = "title"               # 列名(CSV 表头)
selector = "a"               # 可选:相对 row 的选择器;省略=用 row 元素本身
get = "text"
[[data.columns]]
name = "url"
selector = "a"
get = "@href"
# regex / replace / js 每列可选
```
> 每个 `row` 命中的元素产出一行;每列在该行内用 `selector` 定位再 `get` 取值。
> column **不支持** `engine`(无 xpath);其余 `regex`/`replace`/`js` 都支持。

### 6b. JSON API(`format = "json"`)
```toml
format = "json"

[data]
row = "$.data[*]"            # 行用 JSONPath
[[data.columns]]
name = "title"
get = "$.title"             # 列用 JSONPath(相对每个 row 元素)
[[data.columns]]
name = "id"
get = "$.id"
```
> ⚠️ JSON 模式下 column 的 `get` 是 **JSONPath**,且 `regex`/`replace` 生效、**`js` 不生效**(已知限制)。
> 配合 `[pagination]` 的 `query` 抓分页 API。

---

## 7. `[pagination]` 翻页(可选,三类)

```toml
# 1) 查询参数翻页:?page=1..N
[pagination]
type = "query"
param = "page"      # 拼成 ...?page=1, ?page=2 ...(已有 ? 则用 &)
start = 1
end = 10

# 2) 路径翻页:/list/2/  (路径末尾的 /数字/)
[pagination]
type = "path"
start = 1           # 第 1 页 = 去掉末尾数字段的 base;其余 = base + page + "/"
end = 10

# 3) 跟随"下一页"链接
[pagination]
type = "next_link"
next = "a.next"     # 下一页链接选择器(取 href),默认 "a.next"
max = 20            # 最多翻多少页,默认 10
```

---

## 8. 真实样例(照此风格,尽量精简)

**图片 + 懒加载回退链:**
```toml
name = "草榴图片"
type = "image"
domains = ["t66y.com"]
match = "/16/"

[image]
container = ".tpc_content"
images = [
    { selector = "img", get = "@data-src" },
    { selector = "img", get = "@ess-data" },
    { selector = "img", get = "@src" },
]
```

**小说正文 + 繁转简 + 翻页:**
```toml
name = "sosing小说"
type = "text"
domains = ["sosing.com"]

[text]
content = ".entry-content"
get = "text"
convert = "simplify"

[pagination]
type = "path"
start = 1
end = 10
```

**列表导出两列 CSV:**
```toml
name = "DLL-Files"
type = "data"
domains = ["dll-files.com"]
match = "/a/"

[data]
row = "a[href$='.dll.html']"
[[data.columns]]
name = "name"
get = "text"
[[data.columns]]
name = "url"
get = "@href"
```

---

## 9. 生成前自检清单

- [ ] **优先最快路径**:有 JSON 接口?→ `format="json"`;否则 HTML 默认 **CSS**;净化优先 `regex` 而非 `js`;非必要不用 xpath。
- [ ] `name` / `type` / `domains` 三个必填项齐全。
- [ ] `domains` 与给定链接的 host 一致(只填主域,不带 `https://` 和路径)。
- [ ] 同站多栏目?用 `match` 区分,避免和已有源冲突。
- [ ] 选择器尽量稳:优先 `id`/语义化 class,避免一长串脆弱层级。
- [ ] 图片/链接用 `@src`/`@href` 等属性(自动转绝对 URL),不要手拼域名。
- [ ] 懒加载站点考虑 `combine = "first"` 回退链。
- [ ] 文件名小写连字符、放在 `sources/`、不与现有文件重名。
- [ ] 选择器若靠猜,在文件顶部加 `# TODO 待核对` 注释说明。
- [ ] **取 HTML 只用 `snatch fetch`、验证只用 `snatch test`**——全程不碰任何第三方抓取/浏览工具(见「⛔ 硬性规则」)。
- [ ] **已用 `snatch test <URL> --source <toml>` 跑通,确认 `OK` 且样本就是目标内容**(没跑通别交付)。
- [ ] 写完保持最小:删掉所有用不到的可选字段。
