import { useState } from "react";
import AdminAllTimeMetrics from "../components/admin/AdminAllTimeMetrics";
import AdminTimeframedMetrics from "../components/admin/AdminTimeframedMetrics";
import AdminUserActivityTable, {
  type AdminUserActivitySortKey,
} from "../components/admin/AdminUserActivityTable";
import {
  adminAllTimeMetricsData,
  adminTimeframedMetricsByRange,
  adminTimeframedMetricsMeta,
  adminUsageAnalyticsByRange,
  adminUserActivityMeta,
  adminUserActivityRows,
} from "../components/admin/adminMetricsData";

const AdminDashboard = () => {
  // TODO: Backend wiring: replace placeholder with a value from API/state.
  const [selectedRange, setSelectedRange] = useState(
    adminTimeframedMetricsMeta.selectedRange,
  );
  // TODO: Backend wiring: populate custom range dates from API/state.
  const [customStartDate, setCustomStartDate] = useState(
    adminTimeframedMetricsMeta.customStartDate,
  );
  const [customEndDate, setCustomEndDate] = useState(
    adminTimeframedMetricsMeta.customEndDate,
  );
  const [activitySearch, setActivitySearch] = useState("");
  const [activityPage, setActivityPage] = useState(1);
  const [activitySortKey, setActivitySortKey] =
    useState<AdminUserActivitySortKey>("lastSession");
  const [activitySortDirection, setActivitySortDirection] = useState<
    "asc" | "desc"
  >("desc");
  const pageSize = 6;

  // TODO: Backend wiring: replace static maps with API responses keyed by range.
  const resolvedRange = selectedRange === "Custom" ? "Custom" : selectedRange;
  const timeframedMetrics =
    adminTimeframedMetricsByRange[resolvedRange] ??
    adminTimeframedMetricsByRange["Last 30 days"];
  const usageAnalytics =
    adminUsageAnalyticsByRange[resolvedRange] ??
    adminUsageAnalyticsByRange["Last 30 days"];

  // TODO: Backend wiring: replace with license server API query (page, search, range).
  const filteredActivityRows = adminUserActivityRows.filter((row) => {
    const query = activitySearch.trim().toLowerCase();
    if (!query) return true;
    return (
      row.machineId.toLowerCase().includes(query) ||
      row.userId.toLowerCase().includes(query)
    );
  });
  const parseDate = (value: string) => {
    const parsed = Date.parse(value);
    return Number.isNaN(parsed) ? 0 : parsed;
  };
  const parseDurationMinutes = (value: string) => {
    const hoursMatch = value.match(/(\d+)\s*h/);
    const minutesMatch = value.match(/(\d+)\s*m/);
    const hours = hoursMatch ? Number(hoursMatch[1]) : 0;
    const minutes = minutesMatch ? Number(minutesMatch[1]) : 0;
    return hours * 60 + minutes;
  };
  const sortedActivityRows = [...filteredActivityRows].sort((a, b) => {
    const direction = activitySortDirection === "asc" ? 1 : -1;
    const getValue = (row: (typeof adminUserActivityRows)[number]) => {
      switch (activitySortKey) {
        case "machineId":
          return row.machineId;
        case "userId":
          return row.userId;
        case "activeSince":
          return parseDate(row.activeSince);
        case "lastSession":
          return parseDate(row.lastSession);
        case "totalTime":
          return parseDurationMinutes(row.totalTime);
        case "sessions":
          return row.sessions;
        case "avgDuration":
          return parseDurationMinutes(row.avgDuration);
        case "dailyAvg":
          return Number(row.dailyAvg);
      }
    };
    const aValue = getValue(a);
    const bValue = getValue(b);
    if (typeof aValue === "string" && typeof bValue === "string") {
      return aValue.localeCompare(bValue) * direction;
    }
    return (Number(aValue) - Number(bValue)) * direction;
  });
  const totalPages = Math.max(1, Math.ceil(filteredActivityRows.length / pageSize));
  const pagedActivityRows = sortedActivityRows.slice(
    (activityPage - 1) * pageSize,
    activityPage * pageSize,
  );

  return (
    <div className="min-h-screen w-full bg-background p-6 sm:p-10">
      <div className="w-full max-w-6xl mx-auto space-y-6">
        <div className="rounded-2xl border border-border/60 bg-gradient-card p-6 sm:p-8 shadow-sm">
          <p className="text-xs sm:text-sm uppercase tracking-[0.3em] text-muted-foreground">
            Connected Cluster
          </p>
          <h1 className="mt-2 text-2xl sm:text-4xl font-semibold text-foreground">
            {adminAllTimeMetricsData.clusterName}
          </h1>
          <div className="mt-4 flex flex-wrap items-center gap-3 text-sm text-muted-foreground">
            <div className="flex items-center gap-2 rounded-full border border-border/60 bg-background px-3 py-1">
              <span className="text-[11px] uppercase tracking-[0.2em]">
                Operator
              </span>
              <span className="font-semibold text-foreground">
                v{adminAllTimeMetricsData.operatorVersion}
              </span>
              <span className="text-xs text-muted-foreground">
                {adminAllTimeMetricsData.operatorUpdatedAt}
              </span>
            </div>
            <div className="flex items-center gap-2 rounded-full border border-border/60 bg-background px-3 py-1">
              <span className="text-[11px] uppercase tracking-[0.2em]">Tier</span>
              <span className="font-semibold text-foreground">
                {adminAllTimeMetricsData.tier}
              </span>
            </div>
            <div className="flex items-center gap-2 rounded-full border border-border/60 bg-background px-3 py-1">
              <span className="text-[11px] tracking-[0.2em]">mirrord</span>
              <span className="font-semibold text-foreground">
                v{adminAllTimeMetricsData.mirrordVersion}
              </span>
            </div>
            <button className="rounded-full border border-primary/40 px-3 py-1 text-xs font-semibold text-primary">
              Check for updates
            </button>
          </div>
        </div>
        <AdminAllTimeMetrics data={adminAllTimeMetricsData} />
        <AdminTimeframedMetrics
          data={adminTimeframedMetricsMeta}
          metrics={timeframedMetrics}
          showCiData={adminAllTimeMetricsData.tier.toLowerCase() !== "teams"}
          selectedRange={selectedRange}
          onSelectRange={(range) => {
            // TODO: Backend wiring: trigger API fetch/cache update for this range.
            setSelectedRange(range);
            setActivityPage(1);
          }}
          customStartDate={customStartDate}
          customEndDate={customEndDate}
          onCustomRangeChange={(startDate, endDate) => {
            // TODO: Backend wiring: fetch metrics for custom range.
            setCustomStartDate(startDate);
            setCustomEndDate(endDate);
            setSelectedRange("Custom");
            setActivityPage(1);
          }}
          usageAnalyticsData={usageAnalytics}
        />
        <AdminUserActivityTable
          rows={pagedActivityRows}
          primaryKey={adminUserActivityMeta.primaryKey}
          page={activityPage}
          totalPages={totalPages}
          rangeLabel={resolvedRange}
          searchQuery={activitySearch}
          sortKey={activitySortKey}
          sortDirection={activitySortDirection}
          onSearchChange={(value) => {
            setActivitySearch(value);
            setActivityPage(1);
          }}
          onPageChange={setActivityPage}
          onExport={() => {
            const headers = [
              "machineId",
              "userId",
              "activeSince",
              "lastSession",
              "totalTime",
              "sessions",
              "avgDuration",
              "dailyAvg",
            ];
            const rows = filteredActivityRows.map((row) => [
              row.machineId,
              row.userId,
              row.activeSince,
              row.lastSession,
              row.totalTime,
              row.sessions.toString(),
              row.avgDuration,
              row.dailyAvg,
            ]);
            const csv = [
              headers.join(","),
              ...rows.map((row) =>
                row
                  .map((value) => `"${String(value).replace(/"/g, '""')}"`)
                  .join(","),
              ),
            ].join("\n");
            const blob = new Blob([csv], { type: "text/csv;charset=utf-8;" });
            const url = URL.createObjectURL(blob);
            const link = document.createElement("a");
            link.href = url;
            link.download = "mirrord-user-activity.csv";
            link.click();
            URL.revokeObjectURL(url);
          }}
          onSortChange={(key) => {
            if (activitySortKey === key) {
              setActivitySortDirection((prev) => (prev === "asc" ? "desc" : "asc"));
            } else {
              setActivitySortKey(key);
              setActivitySortDirection("desc");
            }
            setActivityPage(1);
          }}
        />
      </div>
    </div>
  );
};

export default AdminDashboard;
