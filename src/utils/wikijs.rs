use std::collections::HashMap;
use std::fmt::Write;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use gql_client::Client;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use tap::Pipe as _;
use teloxide::utils::html;

use crate::utils::format_to;

/// Get markdown page source from Wiki.js GraphQL API.
pub async fn get_wikijs_page(
    endpoint: &str,
    token: &str,
    path: &str,
) -> Result<String> {
    let client = mk_client(endpoint, token);
    let (locale, path) =
        path.trim_start_matches('/').split_once('/').context("Invalid path")?;

    structstruck::strike! {
        #[strikethrough[derive(Deserialize, Debug)]]
        #[strikethrough[serde(rename_all = "camelCase")]]
        struct Response {
            pages: struct Response1 {
                single_by_path: struct Response2 { content: String }
            }
        }
    }

    let response = make_query::<Response>(
        &client,
        "query($locale: String!, $path: String!) {\
            pages {\
                singleByPath(locale: $locale, path: $path) {\
                    content\
                }\
            }\
        }",
        Some(serde_json::json!({ "locale": locale, "path": path })),
    )
    .await?;
    Ok(response.pages.single_by_path.content)
}

pub struct WikiJsUpdates {
    endpoint: String,
    pages: Vec<IntermediateResult>,
}

struct IntermediateResult {
    path: String,
    title: String,
    authors: Vec<String>,
    actions: Vec<String>,
    current_page_contents: String,
    last_version_id: Option<VersionId>,
    prev_version_id: Option<VersionId>,
    changes: (usize, usize),
}

#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct WikiJsUpdateState {
    last_update: DateTime<Utc>,
    pages: HashMap<PageId, VersionId>,
}

/// Connect to Wiki.js GraphQL API and get summary of recent updates since
/// `previous_check`.  Resulting datetime should be passed to `previous_check`
/// in the next call.
pub async fn get_wikijs_updates(
    endpoint: &str,
    token: &str,
    update_state: Option<WikiJsUpdateState>,
) -> Result<(Option<WikiJsUpdates>, WikiJsUpdateState)> {
    let client = mk_client(endpoint, token);

    let mut recent_pages = updates_step1_get_recent_pages(&client).await?;

    let last_update =
        recent_pages.iter().map(|x| x.updated_at).max().context(
            "Failed to get last update time. The wiki has no pages?",
        )?;

    let Some(mut update_state) = update_state else {
        // This is a first run. Just return the last update time.
        return Ok((
            None,
            WikiJsUpdateState { last_update, pages: HashMap::new() },
        ));
    };

    recent_pages.retain(|page| page.updated_at > update_state.last_update);
    if recent_pages.is_empty() {
        // No updates since last check.
        return Ok((
            None,
            WikiJsUpdateState { last_update, pages: update_state.pages },
        ));
    }

    let mut result = updates_step2_get_page_history_and_last_version(
        &client,
        &mut update_state,
        &recent_pages,
    )
    .await?;

    if result.is_empty() {
        // We had pages with updated "updatedAt" field, but none of them had
        // changes in history.
        return Ok((None, update_state));
    }

    updates_step3_get_action_name_and_previous_version(&client, &mut result)
        .await?;

    Ok((
        Some(WikiJsUpdates {
            endpoint: endpoint.to_string(),
            pages: result
                .into_iter()
                .sorted_by_key(|(id, _)| id.0)
                .map(|(_, x)| x)
                .collect(),
        }),
        update_state,
    ))
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct UpdateRecentPage {
    id: PageId,
    locale: String,
    path: String,
    title: String,
    updated_at: DateTime<Utc>,
}

/// Step 1 of `get_wikijs_updates`. Get list of recently updated pages.
async fn updates_step1_get_recent_pages(
    client: &Client,
) -> Result<Vec<UpdateRecentPage>> {
    structstruck::strike! {
        #[strikethrough[derive(Deserialize, Debug)]]
        #[strikethrough[serde(rename_all = "camelCase")]]
        struct Response {
            pages: struct Response1 { list: Vec<UpdateRecentPage> }
        }
    }

    make_query::<Response>(
        client,
        "{\
            pages {\
                list(limit: 10, orderBy: UPDATED, orderByDirection: DESC) {\
                    id locale path title updatedAt\
                }\
            }\
        }",
        None,
    )
    .await?
    .pages
    .list
    .pipe(Ok)
}

/// Step 2 of `get_wikijs_updates`. For each updated page, get its history and
/// the contents for the latest version.
async fn updates_step2_get_page_history_and_last_version(
    client: &Client,
    update_state: &mut WikiJsUpdateState,
    recent_pages: &[UpdateRecentPage],
) -> Result<HashMap<PageId, IntermediateResult>> {
    // The field `pages.history.trail` doesnt include the latest version, so we
    // need to get it separately through `pages.single`.
    let mut query = "{".to_string();
    for page in recent_pages {
        writeln!(
            query,
            "q{0}: pages {{\
                single(id: {0}) {{ authorName updatedAt content }}\
                history(id: {0}) {{ trail {{\
                    versionId versionDate authorName actionType\
                }} }}\
            }},",
            page.id.0,
        )
        .unwrap();
    }
    query.push('}');

    structstruck::strike! {
        #[strikethrough[derive(Deserialize, Debug)]]
        #[strikethrough[serde(rename_all = "camelCase")]]
        struct Response {
            single: struct Response1 {
                author_name: String,
                updated_at: DateTime<Utc>,
                content: String,
            },
            history: struct Response2 {
                trail: Vec<struct Response3 {
                    version_id: VersionId,
                    version_date: DateTime<Utc>,
                    author_name: String,
                    action_type: String,
                }>,
            }
        }
    }

    let mut response: HashMap<String, Response> =
        make_query(client, &query, None).await?;

    let prev_last_update = update_state.last_update;
    update_state.last_update = response
        .values()
        .map(|x| x.single.updated_at)
        .max()
        .context("Failed to get last update time")?;

    let mut result = HashMap::new();

    for pag in recent_pages {
        let Some(page) = response.remove(&format!("q{}", pag.id.0)) else {
            log::warn!("No `q{}` in response2", pag.id.0);
            continue;
        };

        let last_version_id = page.history.trail.first().map(|x| x.version_id);

        let last_version_id_for_state = last_version_id.unwrap_or(VersionId(0));
        if update_state.pages.get(&pag.id) == Some(&last_version_id_for_state) {
            continue;
        }
        update_state.pages.insert(pag.id, last_version_id_for_state);

        let mut actions = Vec::new();
        let mut authors = Vec::new();

        let mut prev_version = None;
        for trail in page.history.trail.iter().rev() {
            if trail.version_date > prev_last_update {
                push_to_uniq_vec(&mut actions, trail.action_type.clone());
                push_to_uniq_vec(&mut authors, trail.author_name.clone());
            } else {
                prev_version = Some(trail.version_id);
            }
        }
        if page.history.trail.is_empty() {
            // New pages have empty history trail, so we don't know the latest
            // version id and thus cannot reguest latest version id in the next
            // step. So, just handle it here instead.
            push_to_uniq_vec(&mut actions, "initial".to_string());
        }
        push_to_uniq_vec(&mut authors, page.single.author_name);

        result.insert(
            pag.id,
            IntermediateResult {
                path: format!("/{}/{}", &pag.locale, &pag.path),
                title: pag.title.clone(),
                authors,
                actions,
                current_page_contents: page.single.content,
                last_version_id,
                prev_version_id: prev_version,
                changes: (0, 0),
            },
        );
    }

    Ok(result)
}

/// Step 3 of `get_wikijs_updates`. For each page, get:
/// - Action name for last version id (since last action is not included in
///   `pages.history.trail` in step 2)
/// - Content for previous version id (to compare diff)
async fn updates_step3_get_action_name_and_previous_version(
    client: &Client,
    result: &mut HashMap<PageId, IntermediateResult>,
) -> Result<()> {
    let query_last = result
        .iter()
        .sorted_by_key(|(id, _)| id.0)
        .filter_map(|(id, res)| {
            Some(format!(
                "q{}: version(pageId: {}, versionId: {}) {{ action }}",
                id.0, id.0, res.last_version_id?.0
            ))
        })
        .join("\n");

    let query_prev = result
        .iter()
        .sorted_by_key(|(id, _)| id.0)
        .filter_map(|(id, res)| {
            Some(format!(
                "q{}: version(pageId: {}, versionId: {}) {{ content }}",
                id.0, id.0, res.prev_version_id?.0
            ))
        })
        .join("\n");

    let mut query = "{".to_string();
    if !query_last.is_empty() {
        format_to!(query, "last: pages {{{}}}", query_last);
    }
    if !query_prev.is_empty() {
        format_to!(query, "prev: pages {{{}}}", query_prev);
    }
    query.push('}');

    structstruck::strike! {
        #[strikethrough[derive(Deserialize, Debug)]]
        #[strikethrough[serde(rename_all = "camelCase")]]
        struct Response {
            #[serde(default)]
            last: HashMap<String, struct Response1 { action: String }>,
            #[serde(default)]
            prev: HashMap<String, struct Response2 { content: String }>,
        }
    }

    let mut response = if query_last.is_empty() && query_prev.is_empty() {
        // Avoid making empty query.
        Response { last: HashMap::new(), prev: HashMap::new() }
    } else {
        make_query(client, &query, None).await?
    };

    for (page_id, res) in result {
        res.changes = match response.prev.remove(&format!("q{}", page_id.0)) {
            Some(page) => diff_stat(&page.content, &res.current_page_contents),
            None => (res.current_page_contents.len(), 0),
        };
        if let Some(page) = response.last.remove(&format!("q{}", page_id.0)) {
            push_to_uniq_vec(
                &mut res.actions,
                // Need to convert here because `pages.history.trail.actionType`
                // and `pages.version.action` has different notation for same
                // actions.
                match page.action.as_str() {
                    "updated" => "edit".to_string(),
                    "moved" => "move".to_string(),
                    other => other.to_string(),
                },
            );
        }
    }

    Ok(())
}

fn mk_client(endpoint: &str, token: &str) -> Client {
    let endpoint = endpoint.trim_end_matches('/');
    Client::new_with_headers(
        format!("{endpoint}/graphql"),
        HashMap::from([("authorization", format!("Bearer {token}"))]),
    )
}

impl WikiJsUpdates {
    /// Render updates as HTML.
    pub fn to_html(&self) -> String {
        self.pages
            .iter()
            .map(|x| {
                format!(
                    "{} {} by {}{}",
                    html::link(
                        &format!("{}{}", self.endpoint, x.path),
                        &x.title,
                    ),
                    human_readable_join(
                        x.actions.iter().map(|s| humanize_action_type(s))
                    ),
                    human_readable_join(x.authors.iter()),
                    match x.changes {
                        (0, 0) => String::new(),
                        (0, del) => format!(" (-{del})"),
                        (add, 0) => format!(" (+{add})"),
                        (add, del) => format!(" (+{add}, -{del})"),
                    }
                )
            })
            .join("\n")
    }

    /// Iterator over a list of paths of updated pages.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.pages.iter().map(|x| x.path.as_str())
    }
}

fn human_readable_join<S: AsRef<str>, I: ExactSizeIterator<Item = S>>(
    items: I,
) -> String {
    let mut result = String::new();
    let len = items.len();
    for (i, item) in items.enumerate() {
        if i > 0 {
            if len > 2 {
                result.push_str(", ");
            } else {
                result.push(' ');
            }
            if i == len - 1 {
                result.push_str("and ");
            }
        }
        result.push_str(item.as_ref());
    }
    result
}

fn humanize_action_type(action: &str) -> String {
    match action {
        "initial" => "created".to_string(),
        "edit" => "edited".to_string(),
        "move" => "moved".to_string(),
        other => format!("\"{other}\""),
    }
}

fn diff_stat(a: &str, b: &str) -> (usize, usize) {
    let mut additions = 0;
    let mut deletions = 0;
    for chg in similar::TextDiff::from_words(a, b).iter_all_changes() {
        match chg.tag() {
            similar::ChangeTag::Equal => (),
            similar::ChangeTag::Delete => deletions += chg.value().len(),
            similar::ChangeTag::Insert => additions += chg.value().len(),
        }
    }
    (additions, deletions)
}

fn push_to_uniq_vec<T: Eq>(vec: &mut Vec<T>, item: T) {
    if !vec.contains(&item) {
        vec.push(item);
    }
}

async fn make_query<K>(
    client: &Client,
    query: &str,
    vars: Option<serde_json::Value>,
) -> Result<K>
where
    K: for<'de> Deserialize<'de>,
{
    client
        .query_with_vars::<K, _>(query, vars)
        .await
        .map_err(|e| anyhow::anyhow!(e))?
        .context("Failed to get response")
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, Hash)]
struct PageId(u32);

#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, Hash)]
struct VersionId(u32);

#[cfg(test)]
mod tests;

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_human_readable_join() {
        assert_eq!(human_readable_join(Vec::<&str>::new().iter()), "");
        assert_eq!(human_readable_join(["a"].iter()), "a");
        assert_eq!(human_readable_join(["a", "b"].iter()), "a and b");
        assert_eq!(human_readable_join(["a", "b", "c"].iter()), "a, b, and c");
        assert_eq!(
            human_readable_join(["a", "b", "c", "d"].iter()),
            "a, b, c, and d"
        );
    }
}
