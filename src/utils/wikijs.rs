use std::collections::HashMap;
use std::fmt::Write;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gql_client::Client;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use teloxide::utils::html;

/// Get markdown page source from Wiki.js GraphQL API.
pub async fn get_wikijs_page(
    endpoint: &str,
    token: &str,
    path: &str,
) -> Result<String> {
    let client = mk_client(endpoint, token);
    let (locale, path) =
        path.trim_start_matches('/').split_once('/').context("Invalid path")?;
    let response = make_query::<ResponsePage>(
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

struct IntermediateResult {
    link: String,
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
#[allow(clippy::single_char_add_str)] // for consistency
pub async fn get_wikijs_updates(
    endpoint: &str,
    token: &str,
    update_state: Option<WikiJsUpdateState>,
) -> Result<(Option<String>, WikiJsUpdateState)> {
    let client = mk_client(endpoint, token);

    // 1. Get list of recently updated pages
    let mut recent_pages = make_query::<ResponseUpdates1>(
        &client,
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
    .list;

    let last_update =
        recent_pages.iter().map(|x| x.updated_at).max().ok_or_else(|| {
            anyhow::anyhow!(
                "Failed to get last update time. The wiki has no pages?"
            )
        })?;

    // This is a first run. Just return the last update time.
    let Some(mut update_state) = update_state else {
        return Ok((
            None,
            WikiJsUpdateState { last_update, pages: HashMap::new() },
        ));
    };
    let prev_last_update = update_state.last_update;

    recent_pages.retain(|page| page.updated_at > update_state.last_update);
    if recent_pages.is_empty() {
        return Ok((
            None,
            WikiJsUpdateState { last_update, pages: update_state.pages },
        ));
    }

    // 2. For each updated page, get its history.
    // The field `pages.history.trail` doesnt include the latest version, so we
    // need to get it separately through `pages.single`.
    let mut query = "{".to_string();
    for page in &recent_pages {
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
    query.push_str("}");

    let mut response2 =
        make_query::<HashMap<String, ResponseUpdates2>>(&client, &query, None)
            .await?;

    update_state.last_update = response2
        .values()
        .map(|x| x.single.updated_at)
        .max()
        .ok_or_else(|| anyhow::anyhow!("Failed to get last update time"))?;

    let mut result = HashMap::new();

    for pag in &recent_pages {
        let Some(page) = response2.remove(&format!("q{}", pag.id.0)) else {
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
                link: html::link(
                    &format!("{}/{}/{}", endpoint, &pag.locale, &pag.path),
                    &pag.title,
                ),
                authors,
                actions,
                current_page_contents: page.single.content,
                last_version_id,
                prev_version_id: prev_version,
                changes: (0, 0),
            },
        );
    }

    if result.is_empty() {
        // We had pages with updated "updatedAt" field, but none of them had
        // changes in history.
        return Ok((None, update_state));
    }

    // 3. For each page, get:
    //    - Action name for last version id (since last action is not included
    //        in `pages.history.trail` in step 2)
    //    - Content for previous version id (to compare diff)
    let mut response3 = {
        let query_last = result
            .iter()
            .filter_map(|(id, res)| {
                Some(format!(
                    "q{}: version(pageId: {}, versionId: {}) {{ action }}",
                    id.0, id.0, res.last_version_id?.0
                ))
            })
            .join("\n");

        let query_prev = result
            .iter()
            .filter_map(|(id, res)| {
                Some(format!(
                    "q{}: version(pageId: {}, versionId: {}) {{ content }}",
                    id.0, id.0, res.prev_version_id?.0
                ))
            })
            .join("\n");

        let mut query = "{".to_string();
        if !query_last.is_empty() {
            query.push_str("last: pages {");
            query.push_str(&query_last);
            query.push_str("}");
        }
        if !query_prev.is_empty() {
            query.push_str("prev: pages {");
            query.push_str(&query_prev);
            query.push_str("}");
        }
        query.push_str("}");

        if query_last.is_empty() && query_prev.is_empty() {
            // Avoid making empty query.
            ResponseUpdates3 { last: HashMap::new(), prev: HashMap::new() }
        } else {
            make_query::<ResponseUpdates3>(&client, &query, None).await?
        }
    };

    for (page_id, res) in &mut result {
        res.changes = match response3.prev.remove(&format!("q{}", page_id.0)) {
            Some(page) => diff_stat(&page.content, &res.current_page_contents),
            None => (res.current_page_contents.len(), 0),
        };
        if let Some(page) = response3.last.remove(&format!("q{}", page_id.0)) {
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

    let text = result
        .values()
        .map(|x| {
            format!(
                "{} {} by {}{}",
                x.link,
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
        .join("\n");

    Ok((Some(text), update_state))
}

fn mk_client(endpoint: &str, token: &str) -> Client {
    let endpoint = endpoint.trim_end_matches('/');
    Client::new_with_headers(
        format!("{endpoint}/graphql"),
        HashMap::from([("authorization", format!("Bearer {token}"))]),
    )
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
        .ok_or_else(|| anyhow::anyhow!("Failed to get response"))
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, Hash)]
struct PageId(u32);

#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, Hash)]
struct VersionId(u32);

structstruck::strike! {
    #[strikethrough[derive(Deserialize, Debug)]]
    #[strikethrough[serde(rename_all = "camelCase")]]
    #[strikethrough[allow(non_camel_case_types)]] // for consistency
    struct ResponsePage {
        pages: struct ResponsePage_1 {
            single_by_path: struct ResponsePage_2 {
                content: String,
            }
        }
    }
}

structstruck::strike! {
    #[strikethrough[derive(Deserialize, Debug)]]
    #[strikethrough[serde(rename_all = "camelCase")]]
    struct ResponseUpdates1 {
        pages: struct ResponseUpdates1_1 {
            list: Vec<struct ResponseUpdates1_2 {
                id: PageId,
                locale: String,
                path: String,
                title: String,
                updated_at: DateTime<Utc>,
            }>,
        }
    }
}

structstruck::strike! {
    #[strikethrough[derive(Deserialize, Debug)]]
    #[strikethrough[serde(rename_all = "camelCase")]]
    struct ResponseUpdates2 {
        single: struct ResponseUpdates2_1 {
            author_name: String,
            updated_at: DateTime<Utc>,
            content: String,
        },
        history: struct ResponseUpdates2_2 {
            trail: Vec<struct ResponseUpdates2_3 {
                version_id: VersionId,
                version_date: DateTime<Utc>,
                author_name: String,
                action_type: String,
            }>,
        }
    }
}

structstruck::strike! {
    #[strikethrough[derive(Deserialize, Debug)]]
    #[strikethrough[serde(rename_all = "camelCase")]]
    struct ResponseUpdates3 {
        #[serde(default)]
        last: HashMap<String, struct ResponseUpdates3_1 { action: String }>,
        #[serde(default)]
        prev: HashMap<String, struct ResponseUpdates3_2 { content: String }>,
    }
}

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
