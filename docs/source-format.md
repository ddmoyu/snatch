# Snatch 源(Source)格式规范

> 一个网站 = 一个"源" = `sources/` 目录下的一个 `.toml` 文件。
> 加源就丢一个文件,删源就删文件。不兼容旧的平铺 `scraper.toml`。

## 目录结构

```
<程序目录>/
  settings.toml          # 全局设置(下载目录/并发/重试等,见 settings)
  sources/
    dll-files.toml       # 一个源
    caoliu.toml
    photos18.toml
```

加载器启动时扫描 `sources/*.toml`,每个文件解析为一个源。剪贴板出现 URL 时,
按 `domains` + `match` 找到匹配的源,按其 `type` 执行。

## 通用头(所有类型)

```toml
name    = "DLL-Files"            # 源名(显示用)
type    = "data"                # data | text | image
domains = ["dll-files.com"]     # 命中其一即可,子域感知(cn./www. 都算)
match   = "/a/"                 # 可选:URL 含此子串才触发(原 path_contains)
enabled = true                  # 可选,默认 true
output  = "csv"                 # 可选,见各类型默认
delay_ms = 300                  # 可选:多次请求(分页/跟进)之间的间隔,防封
```

## 取值语法(所有类型共用)

一个"字段抽取"= 一个表:

```toml
{ selector = "a.title", get = "text", regex = "\\s+", replace = " " }
```

- `selector` —— CSS 选择器(将来 `engine = "xpath"` 复用此键)。
- `get` —— 取什么:
  - 内容:`text` / `ownText` / `html` / `outerHtml`
  - 属性:`@href` / `@src` / `@data-src` / `@srcset`(`@` 前缀=属性,消除歧义)
  - 默认 `text`。
- `regex` / `replace` —— 可选正则净化(对取到的值做替换,`replace` 默认空=删除)。
- `engine` —— 可选 `css`(默认)/ `xpath`(image 选择器支持)。xpath 表达式自带定位,不吃 `container`/`exclude`;`get` 仍用 `@src` / `text` 等,或直接写属性 xpath `//img/@src`。
- `js` —— 可选 JS 后处理(Boa 纯 Rust 引擎)。脚本里 `result`=当前值、`baseUrl`=页面源;脚本的结果值即新值,出错则保留原值。管线顺序:**定位 → 取值 → 正则 → JS**。例:`js = "result.replace(/[^0-9.]/g,'')"`。

## 分页(可选,所有类型共用)

```toml
[pagination]
type  = "query"        # query | path | next_link
param = "page"         # query: ?param=N
start = 1
end   = 10
# type = "next_link" 时:
next  = "a.next"       # 下一页链接选择器
max   = 20             # 最多翻几页
```

---

## 自定义请求头(`[headers]`,可选)

少数站需要特定 `Referer` / `Cookie` / token(防盗链、年龄门、AJAX 接口)。在源里加:

```toml
[headers]
Referer = "https://site.com/list"        # 覆盖默认的同源 Referer
Cookie  = "over18=1; sid=${SITE_SID}"     # 值支持 ${ENV} 环境变量插值
X-Requested-With = "XMLHttpRequest"
```

- 作用于该源的**所有请求**(页面/章节/详情/分页 + 图片下载)。
- 显式 `Referer` 覆盖自动的同源 Referer;不写就用自动的(已能解决大多数 403)。
- 值里的 `${VAR}` 用环境变量替换 —— **密钥(cookie/token)放环境变量,别明文写进会进 git 的源文件**。
- `User-Agent` 会被忽略(交给内置 Chrome 仿真,保持 UA 与 TLS 指纹一致)。
- 仅支持**静态**头;需要登录流程(POST 换 cookie)的站不在此列。

---

## type = "data" —— 结构化导出 CSV

抓"行 + 列"的结构,导出多列 CSV。

```toml
name    = "DLL-Files"
type    = "data"
domains = ["dll-files.com"]
match   = "/a/"

[data]
container = ".file-index"             # 可选:作用域
row       = "a[href$='.dll.html']"    # 每一"行"对应的元素

# 列:每列在"行元素"内取值;selector 省略 = 用行元素本身
[[data.columns]]
name = "name"
get  = "text"
[[data.columns]]
name = "url"
get  = "@href"
```

- 产物:`<标题>.csv`,表头 = 各列 `name`,每个 `row` 元素一行。
- 列的 `selector` 相对于 `row`;省略则对行元素本身取值。
- `output` 固定 csv。

CSV 输出:
```
name,url
"a3d.dll","https://cn.dll-files.com/a3d.dll.html"
```

### JSON 数据源(`format = "json"`)

抓"前端渲染"站点背后的 JSON 接口(复制 API 的 URL,不是页面 URL):
`row` 是定位数组的 JSONPath,每列 `get` 是相对该行的 JSONPath。

```toml
type    = "data"
format  = "json"             # 把响应当 JSON 解析
domains = ["combot.org"]
match   = "/api/chart/"

[data]
row = "$[*]"                 # JSONPath:行数组
[[data.columns]]
name = "title"
get  = "$.t"                 # JSONPath:相对行取字段
[[data.columns]]
name = "username"
get  = "$.u"
[[data.columns]]
name = "members"
get  = "$.s"

[pagination]                 # API 通常是 offset/limit,用 query 翻页
type  = "query"
param = "offset"
start = 0
end   = 100
```

数字/布尔会转成字符串;`regex`/`replace` 仍可用。

---

## type = "text" —— 文档(小说/新闻/论坛)

一个 text 源 = 一份"文档",有**三种互斥的内容策略**,覆盖 短篇/新闻(single)、
论坛(sections)、长篇小说(chapters):

```toml
type = "text"

[text]
title   = "h1"          # 可选,省略用 <title>
author  = ".author"     # 可选元数据
date    = ".date"       # 可选元数据
convert = "simplify"    # 可选:繁→简
strip   = ["广告语"]     # 可选:删掉的字面串

# —— 三选一 ——

# (a) single:单块正文(短篇/新闻)
content = ".article-body"
get     = "text"        # text(默认) | html(保留排版)

# (b) sections:同页重复多块(论坛楼层/多段)
[text.sections]
each    = ".post"
title   = ".post-author"   # 可选:每块小标题
date    = ".post-time"     # 可选
content = ".post-body"
get     = "text"

# (c) chapters:目录链接逐个跟进(长篇)
[text.chapters]
links   = "#toc a"         # 章节链接(按页面顺序)
title   = "h1"             # 每章页内标题
content = ".chapter-body"
get     = "text"

[pagination]               # single/sections 跨页用;chapters 不用
type = "next_link"
next = "a.next"
max  = 50
```

- 产物:`<标题>.txt`(标题/作者/日期做头部,章节/楼层间加分隔)。将来 `chapters` 直接映射 epub 目录。
- `output`:`txt`(默认)。
- 元数据 author/date 全可选。

---

## type = "image" —— 图源/漫画

抽图,下载文件(或只导出链接 CSV)。

```toml
name    = "Photos18"
type    = "image"
domains = ["photos18.com"]

[image]
container = "#content"               # 可选:作用域
images = [                           # 多个选择器按序尝试,合并去重
  { selector = "img", get = "@data-src" },
  { selector = "img", get = "@src" },
]
exclude = [".ad img", ".thumb img"]  # 可选:排除

[image.detail]                       # 可选:列表→详情页再抓图(原 follow_detail)
link   = "a.thumb"                   # 详情页链接选择器
images = [ { selector = "img", get = "@src" } ]

[pagination]                         # 可选
type  = "query"
param = "page"
start = 1
end   = 5
```

- 产物:图片文件下载到 `<标题>/`。
- `output`:`files`(默认,下载)| `csv`(只导出图片链接列表)。

XPath 选择器(CSS 难表达时):
```toml
[image]
images = [
    { selector = "//div[@id='content']//img", get = "@src", engine = "xpath" },
]
```

---

## 已知限制(后续里程碑补)

- **章节内分页 / 目录页分页**:`chapters` 暂只抓单页目录、单页章节;留 `chapters.pagination` 以后加。
- **字符集**:暂靠响应头自动解码(GBK 误标可能乱码);留 `encoding` 以后加。
- **data 行**:保持页面顺序、不去重(与 image 的排序去重相反);某列取空仍输出该行。

## 与全局设置的边界

源文件只描述"**怎么抓某个站**";"**抓到后存哪/并发多少/重试几次**"仍在
`settings.toml`(`download_dir`、`dir_naming`、`dir_collision`、`max_concurrent`、
`timeout`、`retries`、`clipboard_poll_ms`),与源解耦。

## 首次运行

`sources/` 不存在时,生成一个**示例源**(如上面的 data 示例 + 注释),用户照着改。

## 内部模型(实现参考)

```rust
struct Source {
    name: String,
    kind: SourceKind,            // Data | Text | Image
    domains: Vec<String>,
    r#match: Option<String>,
    output: Option<String>,
    pagination: Option<Pagination>,
    rules: SourceRules,          // enum: Data{..} | Text{..} | Image{..}
}
struct Field { selector: String, get: String, regex: Option<String>, replace: Option<String>, engine: Option<String> }
```

抽取统一走 P0 的管线:`定位(selector/engine) → 取值(get) → 正则(regex/replace)`,
各类型只是组织 `Field` 的方式不同。
