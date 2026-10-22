//! 埋点事件数据访问（REQ-202610-1174）。
//!
//! 写入使用单事务批量插入，重复 `event_id` 通过唯一约束跳过；
//! 查询全部使用绑定参数（`QueryBuilder`），不拼接用户输入。

use sqlx::{QueryBuilder, Sqlite};

use crate::telemetry_events::{
    TelemetryAggregate, TelemetryAverages, TelemetryEvent, TelemetryFilter, TelemetryIngestResult,
    TelemetryPageAggregate,
};

use super::{map_db_error, Repository, RepositoryError};

impl Repository {
    /// 批量写入埋点事件；同一用户的重复 `event_id` 会被跳过。
    pub async fn insert_telemetry_events(
        &self,
        events: &[TelemetryEvent],
    ) -> Result<TelemetryIngestResult, RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let mut accepted = 0usize;
        let mut duplicates = 0usize;
        let mut event_ids = Vec::with_capacity(events.len());

        for event in events {
            let result = sqlx::query(
                "INSERT INTO telemetry_events
                     (id, event_id, user_id, page_id, dwell_time,
                      element_click_rate, misclick_rate, scroll_depth,
                      occurred_at, received_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(user_id, event_id) DO NOTHING",
            )
            .bind(&event.id)
            .bind(&event.event_id)
            .bind(&event.user_id)
            .bind(&event.page_id)
            .bind(event.dwell_time)
            .bind(event.element_click_rate)
            .bind(event.misclick_rate)
            .bind(event.scroll_depth)
            .bind(event.timestamp)
            .bind(event.received_at)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;

            if result.rows_affected() == 1 {
                accepted += 1;
                event_ids.push(event.id.clone());
            } else {
                duplicates += 1;
            }
        }

        tx.commit().await.map_err(map_db_error)?;

        Ok(TelemetryIngestResult {
            accepted,
            duplicates,
            event_ids,
        })
    }

    /// 按用户维度查询明细（可按页面与时间过滤），返回当前页与总条数。
    pub async fn list_telemetry_events(
        &self,
        user_id: &str,
        filter: &TelemetryFilter,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<TelemetryEvent>, i64), RepositoryError> {
        let mut count_query = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM telemetry_events");
        push_filters(&mut count_query, user_id, filter);
        let total: i64 = count_query
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await
            .map_err(map_db_error)?;

        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT id, event_id, user_id, page_id, dwell_time,
                    element_click_rate, misclick_rate, scroll_depth,
                    occurred_at AS timestamp, received_at
             FROM telemetry_events",
        );
        push_filters(&mut query, user_id, filter);
        query
            .push(" ORDER BY occurred_at DESC, id DESC LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);

        let items = query
            .build_query_as::<TelemetryEvent>()
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?;

        Ok((items, total))
    }

    /// 按用户维度聚合：总数、总体均值与按页面拆分的均值。
    pub async fn aggregate_telemetry_events(
        &self,
        user_id: &str,
        filter: &TelemetryFilter,
    ) -> Result<TelemetryAggregate, RepositoryError> {
        let mut overall_query = QueryBuilder::<Sqlite>::new(
            "SELECT COUNT(*) AS event_count,
                    COALESCE(AVG(dwell_time), 0.0) AS avg_dwell_time,
                    COALESCE(AVG(element_click_rate), 0.0) AS avg_element_click_rate,
                    COALESCE(AVG(misclick_rate), 0.0) AS avg_misclick_rate,
                    COALESCE(AVG(scroll_depth), 0.0) AS avg_scroll_depth
             FROM telemetry_events",
        );
        push_filters(&mut overall_query, user_id, filter);
        let overall: OverallAggregateRow = overall_query
            .build_query_as()
            .fetch_one(&self.pool)
            .await
            .map_err(map_db_error)?;

        let mut pages_query = QueryBuilder::<Sqlite>::new(
            "SELECT page_id,
                    COUNT(*) AS event_count,
                    COALESCE(AVG(dwell_time), 0.0) AS avg_dwell_time,
                    COALESCE(AVG(element_click_rate), 0.0) AS avg_element_click_rate,
                    COALESCE(AVG(misclick_rate), 0.0) AS avg_misclick_rate,
                    COALESCE(AVG(scroll_depth), 0.0) AS avg_scroll_depth
             FROM telemetry_events",
        );
        push_filters(&mut pages_query, user_id, filter);
        pages_query.push(" GROUP BY page_id ORDER BY event_count DESC, page_id ASC");

        let page_rows = pages_query
            .build_query_as::<PageAggregateRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?;

        Ok(TelemetryAggregate {
            user_id: user_id.to_string(),
            from: filter.from,
            to: filter.to,
            total_events: overall.event_count,
            averages: overall.into_averages(),
            pages: page_rows
                .into_iter()
                .map(PageAggregateRow::into_page)
                .collect(),
        })
    }
}

/// 把用户 / 页面 / 时间过滤条件追加到查询。
fn push_filters(query: &mut QueryBuilder<'_, Sqlite>, user_id: &str, filter: &TelemetryFilter) {
    query
        .push(" WHERE user_id = ")
        .push_bind(user_id.to_string());

    if let Some(page_id) = &filter.page_id {
        query.push(" AND page_id = ").push_bind(page_id.clone());
    }
    if let Some(from) = filter.from {
        query.push(" AND occurred_at >= ").push_bind(from);
    }
    if let Some(to) = filter.to {
        query.push(" AND occurred_at <= ").push_bind(to);
    }
}

#[derive(Debug, sqlx::FromRow)]
struct OverallAggregateRow {
    event_count: i64,
    avg_dwell_time: f64,
    avg_element_click_rate: f64,
    avg_misclick_rate: f64,
    avg_scroll_depth: f64,
}

impl OverallAggregateRow {
    fn into_averages(self) -> TelemetryAverages {
        TelemetryAverages {
            avg_dwell_time: self.avg_dwell_time,
            avg_element_click_rate: self.avg_element_click_rate,
            avg_misclick_rate: self.avg_misclick_rate,
            avg_scroll_depth: self.avg_scroll_depth,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PageAggregateRow {
    page_id: String,
    event_count: i64,
    avg_dwell_time: f64,
    avg_element_click_rate: f64,
    avg_misclick_rate: f64,
    avg_scroll_depth: f64,
}

impl PageAggregateRow {
    fn into_page(self) -> TelemetryPageAggregate {
        TelemetryPageAggregate {
            page_id: self.page_id,
            event_count: self.event_count,
            averages: TelemetryAverages {
                avg_dwell_time: self.avg_dwell_time,
                avg_element_click_rate: self.avg_element_click_rate,
                avg_misclick_rate: self.avg_misclick_rate,
                avg_scroll_depth: self.avg_scroll_depth,
            },
        }
    }
}
