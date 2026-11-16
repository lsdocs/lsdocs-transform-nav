//! First-party site navigation transformer.
//!
//! The collect pass records each document's title and navigation settings;
//! the apply pass gives every document a `nav` metadata entry describing the
//! whole site from that page's point of view, which page templates turn into
//! a sidebar and previous/next links:
//!
//! ```json
//! {
//!   "items": [
//!     {"title": "Home", "url": "../index.md"},
//!     {"title": "Guide", "open": true, "children": [
//!       {"title": "Install", "url": "install.md", "current": true}
//!     ]}
//!   ],
//!   "prev": {"title": "Home", "url": "../index.md"},
//!   "next": null
//! }
//! ```
//!
//! The tree follows the source directories. A directory's `index` page
//! (`index.md`, or any `index.*` document) titles and links its section;
//! without one the section is named after the directory. The site's root
//! `index` page comes first. Each linked item has the page's `source` path,
//! from which renderers build the link to its output (`render-html` has a
//! `page_url` template filter for this), and a `url` relative to the
//! current page that points at the source file.
//!
//! If the root `index` page lists its children (`nav_children`), the tree
//! follows those lists instead: the root's children, each page's own
//! children below it, and pages no list names left out. `sphinx-addons`
//! fills `nav_children` from toctrees; front matter can set it too.
//!
//! Front matter:
//!
//! | Key | Meaning |
//! |---|---|
//! | `nav_title` | Title in the navigation, instead of the page title. |
//! | `nav_order` | Sort key among siblings; pages without one come after, by file name. |
//! | `nav_exclude` | `true` leaves the page out of the navigation. |
//! | `nav_children` | The pages below this one, in order: source paths relative to it, or to the source directory with a leading `/`. |

use std::collections::BTreeMap;

use lsdocs_plugin_sdk::protocol::{
    TransformApplyParams, TransformApplyResult, TransformCollectParams, TransformCollectResult,
};
use lsdocs_plugin_sdk::{Context, Plugin, PluginResult, Transformer, relative_path};
use serde_json::{Map, Value, json};

/// The plugin, as registered with the host.
pub fn plugin() -> Plugin {
    Plugin::new("transform-nav", env!("CARGO_PKG_VERSION"))
        .transformer(Nav)
        .cacheable()
}

/// The navigation transformer.
#[derive(Debug, Clone, Copy, Default)]
pub struct Nav;

impl Transformer for Nav {
    const COLLECTS: bool = true;

    async fn collect(
        &self,
        params: TransformCollectParams,
        _ctx: &Context,
    ) -> PluginResult<TransformCollectResult> {
        let doc = &params.document;
        let meta = |key: &str| doc.metadata.get(key);
        let title = meta("nav_title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| doc.title())
            .unwrap_or_else(|| doc.source.clone());
        Ok(TransformCollectResult {
            data: json!({
                "title": title,
                "order": meta("nav_order").filter(|v| v.is_number()),
                "exclude": meta("nav_exclude").and_then(Value::as_bool).unwrap_or(false),
                "children": meta("nav_children")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(Value::as_str)
                            .map(|path| absolute(&doc.source, path))
                            .collect::<Vec<_>>()
                    }),
            }),
            diagnostics: vec![],
        })
    }

    async fn apply(
        &self,
        params: TransformApplyParams,
        _ctx: &Context,
    ) -> PluginResult<TransformApplyResult> {
        let mut document = params.document;
        let pages = params
            .collected
            .as_object()
            .map(|pages| {
                pages
                    .iter()
                    .filter(|(_, page)| page["exclude"] != true)
                    .map(|(source, page)| Page {
                        source,
                        title: page["title"].as_str().unwrap_or(source),
                        order: page["order"].as_f64(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let tree = Section::build(&pages);
        let listed = tree.index.and_then(|home| {
            let collected = params.collected.as_object()?;
            let children = collected.get(home.source)?["children"].as_array()?;
            (!children.is_empty()).then_some(home)
        });
        let mut items = match (listed, params.collected.as_object()) {
            (Some(home), Some(collected)) => {
                let by_source: BTreeMap<&str, &Page> =
                    pages.iter().map(|p| (p.source, p)).collect();
                let mut seen = std::collections::BTreeSet::from([home.source]);
                listed_items(
                    home.source,
                    collected,
                    &by_source,
                    &document.source,
                    &mut seen,
                )
            }
            _ => tree.items(&document.source),
        };
        // The root index page leads, as the site's home.
        if let Some(home) = &tree.index {
            items.insert(0, home.item(&document.source));
        }
        let mut flat = Vec::new();
        flatten(&items, &mut flat);
        let at = flat.iter().position(|item| item["current"] == true);
        let neighbor = |i: Option<usize>| {
            i.and_then(|i| flat.get(i))
                .map(|item| {
                    json!({"title": item["title"], "url": item["url"], "source": item["source"]})
                })
                .unwrap_or(Value::Null)
        };
        let nav = json!({
            "items": items,
            "prev": neighbor(at.and_then(|i| i.checked_sub(1))),
            "next": neighbor(at.map(|i| i + 1)),
        });
        document.metadata.insert("nav".into(), nav);
        Ok(TransformApplyResult {
            document,
            diagnostics: vec![],
        })
    }
}

/// A document as the navigation sees it.
#[derive(Debug, Clone, Copy)]
struct Page<'a> {
    source: &'a str,
    title: &'a str,
    order: Option<f64>,
}

impl Page<'_> {
    /// The page's navigation item, seen from the page `from`.
    fn item(&self, from: &str) -> Value {
        let mut item = Map::new();
        item.insert("title".into(), json!(self.title));
        item.insert("url".into(), json!(relative_path(from, self.source)));
        item.insert("source".into(), json!(self.source));
        if self.source == from {
            item.insert("current".into(), json!(true));
        }
        Value::Object(item)
    }
}

/// A directory of the source tree.
#[derive(Debug, Default)]
struct Section<'a> {
    /// The directory's `index` page.
    index: Option<Page<'a>>,
    /// Other pages directly in the directory.
    pages: Vec<Page<'a>>,
    /// Subdirectories, by name.
    dirs: BTreeMap<&'a str, Section<'a>>,
}

impl<'a> Section<'a> {
    fn build(pages: &[Page<'a>]) -> Self {
        let mut root = Section::default();
        for page in pages {
            let mut parts: Vec<&str> = page.source.split('/').collect();
            let file = parts.pop().unwrap_or_default();
            let mut section = &mut root;
            for dir in parts {
                section = section.dirs.entry(dir).or_default();
            }
            let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
            if stem == "index" && section.index.is_none() {
                section.index = Some(*page);
            } else {
                section.pages.push(*page);
            }
        }
        root
    }

    fn order(&self) -> Option<f64> {
        self.index.and_then(|p| p.order)
    }

    /// The section's child items, sorted, seen from the page `from`.
    fn items(&self, from: &str) -> Vec<Value> {
        // (order, file or directory name, item)
        let mut entries: Vec<(Option<f64>, &str, Value)> = Vec::new();
        for page in &self.pages {
            let name = page.source.rsplit('/').next().unwrap_or(page.source);
            entries.push((page.order, name, page.item(from)));
        }
        for (name, dir) in &self.dirs {
            let children = dir.items(from);
            if children.is_empty() && dir.index.is_none() {
                continue;
            }
            let mut item = match &dir.index {
                Some(index) => index.item(from),
                None => json!({ "title": pretty(name) }),
            };
            if let Some(map) = item.as_object_mut() {
                if map.contains_key("current") || children.iter().any(contains_current) {
                    map.insert("open".into(), json!(true));
                }
                if !children.is_empty() {
                    map.insert("children".into(), Value::Array(children));
                }
            }
            entries.push((dir.order(), name, item));
        }
        entries.sort_by(|a, b| {
            let key = |o: Option<f64>| (o.is_none(), o.unwrap_or(0.0));
            let (a_missing, a_order) = key(a.0);
            let (b_missing, b_order) = key(b.0);
            a_missing
                .cmp(&b_missing)
                .then(a_order.total_cmp(&b_order))
                .then(a.1.cmp(b.1))
        });
        entries.into_iter().map(|(_, _, item)| item).collect()
    }
}

/// The items of the pages `parent` lists in `nav_children`, each with the
/// pages it lists below it, seen from the page `from`. A page is listed
/// once, where it is first named.
fn listed_items<'a>(
    parent: &str,
    collected: &'a Map<String, Value>,
    pages: &BTreeMap<&'a str, &Page<'a>>,
    from: &str,
    seen: &mut std::collections::BTreeSet<&'a str>,
) -> Vec<Value> {
    let mut items = Vec::new();
    let children = collected
        .get(parent)
        .and_then(|p| p["children"].as_array())
        .cloned()
        .unwrap_or_default();
    for child in children {
        let Some(child) = child.as_str() else {
            continue;
        };
        let Some((&source, page)) = pages.get_key_value(child) else {
            continue;
        };
        if !seen.insert(source) {
            continue;
        }
        let below = listed_items(source, collected, pages, from, seen);
        let mut item = page.item(from);
        if let Some(map) = item.as_object_mut()
            && !below.is_empty()
        {
            if map.contains_key("current") || below.iter().any(contains_current) {
                map.insert("open".into(), json!(true));
            }
            map.insert("children".into(), Value::Array(below));
        }
        items.push(item);
    }
    items
}

/// `path` as a source path: relative to the page `from`, or to the source
/// directory with a leading `/`.
fn absolute(from: &str, path: &str) -> String {
    let joined = match path.strip_prefix('/') {
        Some(rest) => rest.to_owned(),
        None => match from.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/{path}"),
            None => path.to_owned(),
        },
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

fn contains_current(item: &Value) -> bool {
    item["current"] == true || item["open"] == true
}

/// Pages in display order, depth first.
fn flatten<'v>(items: &'v [Value], out: &mut Vec<&'v Value>) {
    for item in items {
        if item.get("url").is_some() {
            out.push(item);
        }
        if let Some(children) = item["children"].as_array() {
            flatten(children, out);
        }
    }
}

/// A directory name as a title: `getting-started` -> `Getting started`.
fn pretty(name: &str) -> String {
    let spaced = name.replace(['-', '_'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => spaced,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lsdocs_plugin_sdk::doctree::{Document, Heading, Node, text};
    use lsdocs_plugin_sdk::protocol::InitializeParams;
    use lsdocs_plugin_sdk::testing;

    fn doc(source: &str, title: &str, front: Value) -> Document {
        let mut doc = Document::new(source);
        doc.children = vec![Node::Heading(Heading {
            level: 1,
            children: vec![text(title)],
            ..Heading::default()
        })];
        if let Value::Object(front) = front {
            doc.metadata.extend(front);
        }
        doc
    }

    /// The tree as `title(url)` lines, indented by depth, with `*` on the
    /// current page and `+` on open sections.
    fn outline(items: &Value, depth: usize, out: &mut Vec<String>) {
        for item in items.as_array().unwrap() {
            let mut line = format!("{}{}", "  ".repeat(depth), item["title"].as_str().unwrap());
            if let Some(url) = item["url"].as_str() {
                line.push_str(&format!("({url})"));
            }
            if item["current"] == true {
                line.push('*');
            }
            if item["open"] == true {
                line.push('+');
            }
            out.push(line);
            if let Some(children) = item.get("children") {
                outline(children, depth + 1, out);
            }
        }
    }

    #[tokio::test]
    async fn builds_a_tree_per_page() {
        let docs = vec![
            doc("index.md", "Welcome", json!({"nav_title": "Home"})),
            doc("about.md", "About", json!({})),
            doc("guide/index.md", "The guide", json!({"nav_order": 1})),
            doc("guide/b-install.md", "Install", json!({"nav_order": 1})),
            doc("guide/a-usage.md", "Usage", json!({"nav_order": 2})),
            doc("guide/z-faq.md", "FAQ", json!({})),
            doc("reference/getting-started/cli.md", "CLI", json!({})),
            doc("secret.md", "Secret", json!({"nav_exclude": true})),
        ];
        let out = testing::run_transform(&plugin(), &InitializeParams::default(), docs)
            .await
            .unwrap();
        let (install, _) = &out[3];
        let nav = &install.metadata["nav"];
        let mut lines = Vec::new();
        outline(&nav["items"], 0, &mut lines);
        assert_eq!(
            lines,
            [
                "Home(../index.md)",
                "The guide(index.md)+",
                "  Install(b-install.md)*",
                "  Usage(a-usage.md)",
                "  FAQ(z-faq.md)",
                "About(../about.md)",
                "Reference",
                "  Getting started",
                "    CLI(../reference/getting-started/cli.md)",
            ]
        );
        assert_eq!(
            nav["prev"],
            json!({"title": "The guide", "url": "index.md", "source": "guide/index.md"})
        );
        assert_eq!(nav["next"]["url"], "a-usage.md");

        let (home, _) = &out[0];
        assert_eq!(home.metadata["nav"]["prev"], Value::Null);
        assert_eq!(home.metadata["nav"]["items"][0]["current"], true);
        // Excluded pages still get the navigation, but are not in it.
        let (secret, _) = &out[7];
        assert!(!secret.metadata["nav"].to_string().contains("Secret"));
    }

    #[tokio::test]
    async fn listed_children_replace_the_directories() {
        let docs = vec![
            doc(
                "index.md",
                "Home",
                json!({"nav_children": ["guide/intro.md", "/about.md", "about.md"]}),
            ),
            doc("about.md", "About", json!({})),
            doc(
                "guide/intro.md",
                "Intro",
                json!({"nav_children": ["../ref/deep.md", "missing.md"]}),
            ),
            doc("ref/deep.md", "Deep", json!({})),
            doc("unlisted.md", "Unlisted", json!({})),
        ];
        let out = testing::run_transform(&plugin(), &InitializeParams::default(), docs)
            .await
            .unwrap();
        let (deep, _) = &out[3];
        let mut lines = Vec::new();
        outline(&deep.metadata["nav"]["items"], 0, &mut lines);
        assert_eq!(
            lines,
            [
                "Home(../index.md)",
                "Intro(../guide/intro.md)+",
                "  Deep(deep.md)*",
                "About(../about.md)",
            ]
        );
        assert_eq!(deep.metadata["nav"]["prev"]["title"], "Intro");
        assert_eq!(deep.metadata["nav"]["next"]["title"], "About");
    }

    #[test]
    fn pretty_names() {
        assert_eq!(pretty("getting-started"), "Getting started");
        assert_eq!(pretty("api_ref"), "Api ref");
        assert_eq!(pretty("élan"), "Élan");
    }
}
