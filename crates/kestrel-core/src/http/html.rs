// kestrel :: http/html — DOM document over the `scraper` crate, exposing the
// kestrel lite-engine surface: select/text/attr/links/images/tables/forms/meta/
// jsonld/readable. The readability renderer matches the JS engine's output.
use once_cell::sync::Lazy;
use regex::Regex;
use scraper::{ElementRef, Html, Selector};

pub struct HtmlDoc {
    html: Html,
    base: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Link {
    pub text: String,
    pub href: String,
}

fn resolve_url(base: &str, href: &str) -> String {
    if href.is_empty()
        || href.starts_with("http://")
        || href.starts_with("https://")
        || href.starts_with("data:")
        || href.starts_with("javascript:")
        || href.starts_with('#')
    {
        return href.to_string();
    }
    if let Ok(b) = url::Url::parse(base)
        && let Ok(j) = b.join(href)
    {
        return j.to_string();
    }
    href.to_string()
}

/// Map the kestrel selector shorthands onto CSS where cheap (`id=x` → `#x`,
/// `tag=div` → `div`); everything else goes to scraper's CSS parser as-is.
fn css(sel: &str) -> String {
    if let Some(rest) = sel.strip_prefix("id=") {
        return format!("#{rest}");
    }
    if let Some(rest) = sel.strip_prefix("tag=") {
        return rest.to_string();
    }
    sel.to_string()
}

fn select_all<'a>(doc: &'a Html, sel: &str) -> Vec<ElementRef<'a>> {
    match Selector::parse(&css(sel)) {
        Ok(selector) => doc.select(&selector).collect(),
        Err(_) => vec![],
    }
}

impl HtmlDoc {
    pub fn parse(html: &str, base: &str) -> HtmlDoc {
        HtmlDoc {
            html: Html::parse_document(html),
            base: base.to_string(),
        }
    }

    pub fn text_of(&self, sel: &str) -> Option<String> {
        let doc = &self.html;
        select_all(doc, sel).first().map(|el| text_content(*el))
    }

    pub fn attr_of(&self, sel: &str, name: &str) -> Option<String> {
        let doc = &self.html;
        select_all(doc, sel)
            .first()
            .and_then(|el| el.value().attr(name))
            .map(|s| s.to_string())
    }

    pub fn html_of(&self, sel: &str) -> Option<String> {
        let doc = &self.html;
        select_all(doc, sel).first().map(|el| el.inner_html())
    }

    pub fn title(&self) -> Option<String> {
        self.text_of("title")
    }

    pub fn links(&self) -> Vec<Link> {
        let doc = &self.html;
        let mut out = vec![];
        if let Ok(sel) = Selector::parse("a[href]") {
            for el in doc.select(&sel) {
                let href = el.value().attr("href").unwrap_or("");
                if href.starts_with("javascript:") || href.starts_with('#') {
                    continue;
                }
                out.push(Link {
                    text: text_content(el),
                    href: resolve_url(&self.base, href),
                });
            }
        }
        out
    }

    pub fn images(&self) -> Vec<serde_json::Value> {
        let doc = &self.html;
        if let Ok(sel) = Selector::parse("img") {
            doc.select(&sel)
                .map(|el| {
                    serde_json::json!({
                        "src": resolve_url(&self.base, el.value().attr("src").unwrap_or("")),
                        "alt": el.value().attr("alt").unwrap_or(""),
                    })
                })
                .collect()
        } else {
            vec![]
        }
    }

    pub fn tables(&self) -> Vec<serde_json::Value> {
        let doc = &self.html;
        let mut out = vec![];
        if let Ok(sel) = Selector::parse("table") {
            for table in doc.select(&sel) {
                let mut headers: Vec<String> = vec![];
                let mut rows: Vec<Vec<String>> = vec![];
                if let Ok(thead_sel) = Selector::parse("thead tr")
                    && let Some(thead) = table.select(&thead_sel).next()
                    && let Ok(cell_sel) = Selector::parse("th, td")
                {
                    headers = thead.select(&cell_sel).map(text_content).collect();
                }
                if let Ok(tr_sel) = Selector::parse("tr") {
                    for tr in table.select(&tr_sel) {
                        if let Ok(cell_sel) = Selector::parse("td, th") {
                            let cells: Vec<String> =
                                tr.select(&cell_sel).map(text_content).collect();
                            if cells.is_empty() {
                                continue;
                            }
                            if headers.is_empty()
                                && rows.is_empty()
                                && let Ok(th_sel) = Selector::parse("th")
                                && tr.select(&th_sel).next().is_some()
                            {
                                headers = cells;
                                continue;
                            }
                            rows.push(cells);
                        }
                    }
                }
                out.push(serde_json::json!({ "headers": headers, "rows": rows }));
            }
        }
        out
    }

    pub fn forms(&self) -> Vec<serde_json::Value> {
        let doc = &self.html;
        let mut out = vec![];
        if let Ok(form_sel) = Selector::parse("form") {
            for form in doc.select(&form_sel) {
                let action = form.value().attr("action").unwrap_or("");
                let method = form.value().attr("method").unwrap_or("get");
                let fields: Vec<serde_json::Value> = Selector::parse("input, textarea, select")
                    .map(|f_sel| {
                        form.select(&f_sel)
                            .map(|f| {
                                let v = f.value();
                                serde_json::json!({
                                    "tag": v.name(),
                                    "name": v.attr("name").unwrap_or(""),
                                    "type": v.attr("type").unwrap_or(""),
                                    "value": v.attr("value").unwrap_or(""),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(serde_json::json!({
                    "action": resolve_url(&self.base, action),
                    "method": method.to_lowercase(),
                    "fields": fields,
                }));
            }
        }
        out
    }

    pub fn meta(&self) -> std::collections::HashMap<String, String> {
        let doc = &self.html;
        let mut out = std::collections::HashMap::new();
        if let Ok(t_sel) = Selector::parse("title")
            && let Some(t) = doc.select(&t_sel).next()
        {
            out.insert("title".to_string(), text_content(t));
        }
        if let Ok(m_sel) = Selector::parse("meta") {
            for m in doc.select(&m_sel) {
                let v = m.value();
                let name = v.attr("name").or_else(|| v.attr("property")).unwrap_or("");
                let content = v.attr("content").unwrap_or("");
                if !name.is_empty() && !content.is_empty() {
                    out.insert(name.to_string(), content.to_string());
                }
            }
        }
        out
    }

    pub fn jsonld(&self) -> Vec<serde_json::Value> {
        let doc = &self.html;
        Selector::parse("script[type=\"application/ld+json\"]")
            .map(|sel| {
                doc.select(&sel)
                    .filter_map(|s| serde_json::from_str(&s.text().collect::<String>()).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Readability renderer — same rules as the JS engine: headings become
    /// markdown, paragraphs separated, list items as `- `, images by alt.
    pub fn readable(&self) -> String {
        let mut out = String::new();
        walk_readable(&self.html.root_element(), &mut out);
        let body = collapse_blanks(&out);
        // title first, like the JS facade renders `# {title}\n\n{body}`
        let title = select_all(&self.html, "title")
            .first()
            .map(|el| text_content(*el));
        match title {
            Some(t) if !t.is_empty() && !body.starts_with(&format!("# {t}")) => {
                format!("# {t}\n\n{body}")
            }
            _ => body,
        }
    }
}

fn walk_readable(node: &ego_tree::NodeRef<'_, scraper::node::Node>, out: &mut String) {
    static SKIP: Lazy<Regex> =
        Lazy::new(|| Regex::new("(?i)^(script|style|noscript|template|svg|head)$").unwrap());
    for child in node.children() {
        if let Some(el) = ElementRef::wrap(child) {
            let name = el.value().name();
            if SKIP.is_match(name) {
                continue;
            }
            match name {
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    let level = name[1..].parse::<usize>().unwrap_or(1);
                    let text = text_content(el);
                    if !text.is_empty() {
                        out.push_str(&format!("\n\n{} {}\n\n", "#".repeat(level), text));
                    }
                }
                "p" => {
                    let text = text_content(el);
                    if !text.is_empty() {
                        out.push_str(&format!("\n{text}\n"));
                    }
                }
                "li" => {
                    let text = text_content(el);
                    out.push_str(&format!("\n- {text}"));
                }
                "br" => out.push('\n'),
                "img" => {
                    if let Some(alt) = el.value().attr("alt")
                        && !alt.is_empty()
                    {
                        out.push_str(&format!(" [img: {alt}] "));
                    }
                }
                _ => walk_readable(&child, out),
            }
        }
    }
}

fn collapse_blanks(s: &str) -> String {
    static MANY_NL: Lazy<Regex> = Lazy::new(|| Regex::new(r"\n{3,}").unwrap());
    static MANY_SP: Lazy<Regex> = Lazy::new(|| Regex::new(r"[ \t]+").unwrap());
    let s = MANY_NL.replace_all(s, "\n\n");
    let s = MANY_SP.replace_all(&s, " ");
    s.trim().to_string()
}

/// Deep text content of an element (excludes script/style children).
fn text_content(el: ElementRef) -> String {
    static SKIP: Lazy<Regex> =
        Lazy::new(|| Regex::new("(?i)^(script|style|noscript|template)$").unwrap());
    let mut s = String::new();
    collect_text(el, &mut s, &SKIP);
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_text(el: ElementRef, out: &mut String, skip: &Regex) {
    for child in el.children() {
        match scraper::ElementRef::wrap(child) {
            Some(inner) => {
                if !skip.is_match(inner.value().name()) {
                    collect_text(inner, out, skip);
                }
            }
            None => {
                if let Some(t) = child.value().as_text() {
                    out.push_str(t);
                    out.push(' ');
                }
            }
        }
    }
}
