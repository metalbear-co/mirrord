# Admin Dashboard Wiring Guide

This folder is split into presentational components and data contracts so backend wiring is fast and low-risk.

## File Map

- `AdminDashboard.tsx`: Page orchestration + state for time ranges, paging, sorting, export.
- `AdminAllTimeMetrics.tsx`: All-time KPI cards + calculator modal.
- `AdminTimeframedMetrics.tsx`: Timeframe selector + KPI cards + embedded analytics chart.
- `AdminUsageAnalytics.tsx`: Per-user bar chart (sorted by active metric).
- `AdminUserActivityTable.tsx`: User activity table, search, pagination, export.
- `adminMetricsData.ts`: Data contracts + placeholder datasets.

## Data Contracts (map API responses here)

All backend integrations should hydrate these shapes:

- `AdminAllTimeMetricsData`
- `AdminTimeframedMetricsMeta`
- `AdminTimeframedMetricsValues`
- `AdminUsageAnalyticsData`
- `AdminUserActivityRow`
- `AdminUserActivityMeta` (controls primary key behavior)

## Time Range Wiring

1. UI range selection is owned in `AdminDashboard.tsx`.
2. Replace `adminTimeframedMetricsByRange` and `adminUsageAnalyticsByRange` with API results keyed by range.
3. When `selectedRange === "Custom"`, include `customStartDate`/`customEndDate` in the API request.

## User Activity Table Wiring

### Primary key selection

The table can group by a primary key. Supported options (v3.130.0+):

- `machine_id` (default)
- `username` (kubernetes username)
- `client_username`
- `client_hostname`

Set it in `adminUserActivityMeta.primaryKey` and keep `machineId`/`userId` fields populated.
When primaryKey is not `machine_id`, the UI will show `userId` as the main label and `machineId` as secondary.

### Data and pagination

Replace `adminUserActivityRows` with server data. Expected inputs:

- `page`, `pageSize`
- `searchQuery`
- `range` (selected range or custom dates)

### Export

`AdminDashboard.tsx` currently exports client-side CSV from the filtered rows.
For real data, wire `onExport` to a license server CSV endpoint.

## Calculator Wiring

In `AdminAllTimeMetrics.tsx`:

- `avgUserSessionMinutes` and `avgCiWaitMinutes` are local state.
- Map persisted values into those initial states.
- `totalUserSessions`/`totalCiSessions` are used to compute the visible totals.

## CI Gating

If tier is `Teams`, CI-related fields are hidden. This is controlled in:

- `AdminAllTimeMetrics.tsx`
- `AdminTimeframedMetrics.tsx` (`showCiData` prop)

