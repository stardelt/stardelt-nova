//! The catalog documentation index (design ticket stardelt-nova#104, search slice 1).

use sqlx::PgPool;

use crate::catalog_docs::TableDocumentation;

/// The text the search slice matches word by word: `namespace.table`, the description,
/// every tag, the owner and the domain, lowercased and space separated.
pub fn search_text(namespace: &str, table: &str, doc: &TableDocumentation) -> String {
    let mut parts = vec![format!("{namespace}.{table}")];
    if let Some(description) = &doc.description {
        parts.push(description.clone());
    }
    parts.extend(doc.tags.iter().cloned());
    if let Some(owner) = &doc.owner {
        parts.push(owner.clone());
    }
    if let Some(domain) = &doc.domain {
        parts.push(domain.clone());
    }
    parts.join(" ").to_lowercase()
}

/// Record (or refresh) the row for one table. **Best effort**: an edit must not fail
/// because the index could not be written; the failure is logged and swallowed.
pub async fn record(
    pool: Option<&PgPool>,
    catalog: &str,
    namespace: &str,
    table: &str,
    doc: &TableDocumentation,
) {
    let Some(pool) = pool else { return };
    let columns = serde_json::to_value(&doc.columns).unwrap_or(serde_json::json!([]));
    let result = sqlx::query(
        "INSERT INTO catalog_doc (catalog, namespace, table_name, description, tags, owner, \
             domain, column_docs, search_text) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (catalog, namespace, table_name) DO UPDATE SET \
             description = EXCLUDED.description, tags = EXCLUDED.tags, owner = EXCLUDED.owner, \
             domain = EXCLUDED.domain, column_docs = EXCLUDED.column_docs, \
             search_text = EXCLUDED.search_text, updated_at = now()",
    )
    .bind(catalog)
    .bind(namespace)
    .bind(table)
    .bind(&doc.description)
    .bind(&doc.tags)
    .bind(&doc.owner)
    .bind(&doc.domain)
    .bind(&columns)
    .bind(search_text(namespace, table, doc))
    .execute(pool)
    .await;
    if let Err(e) = result {
        tracing::warn!(
            "catalog documentation index not updated for {catalog}.{namespace}.{table}: {e}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> TableDocumentation {
        TableDocumentation {
            description: Some("Daily Revenue per Shop".into()),
            tags: vec!["finance".into(), "daily".into()],
            owner: Some("analytics".into()),
            domain: Some("Sales".into()),
            columns: Vec::new(),
            can_edit: false,
            edit_rule: String::new(),
            can_rollback: false,
            rollback_rule: String::new(),
        }
    }

    #[test]
    fn search_text_lowercases_every_field() {
        let text = search_text("gold", "daily_revenue", &doc());
        assert_eq!(
            text,
            "gold.daily_revenue daily revenue per shop finance daily analytics sales"
        );
    }

    #[tokio::test]
    async fn record_without_a_pool_is_a_no_op() {
        record(None, "gold", "shop", "daily_revenue", &doc()).await;
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn record_writes_one_row_and_a_second_call_refreshes_it() {
        let pool = crate::db::test_support::migrated_pool().await;
        record(Some(&pool), "gold", "shop", "daily_revenue", &doc()).await;
        let mut second = doc();
        second.tags.push("monthly".into());
        record(Some(&pool), "gold", "shop", "daily_revenue", &second).await;
        // The ticket's `(array_agg(tags))[1]` reads NULL: `array_agg` of a `text[]` column
        // builds a two-dimensional array, and PostgreSQL answers NULL for a subscript that
        // names fewer dimensions than the array has. The row's own `tags` is the same value.
        let (rows, tags): (i64, Vec<String>) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM catalog_doc WHERE catalog = 'gold'), tags \
             FROM catalog_doc WHERE catalog = 'gold'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(
            tags,
            vec![
                "finance".to_string(),
                "daily".to_string(),
                "monthly".to_string()
            ]
        );
    }
}
