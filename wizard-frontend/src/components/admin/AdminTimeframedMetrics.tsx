import AdminUsageAnalytics from "./AdminUsageAnalytics";
import type {
  AdminTimeframedMetricsMeta,
  AdminTimeframedMetricsValues,
  AdminUsageAnalyticsData,
} from "./adminMetricsData";

interface AdminTimeframedMetricsProps {
  data: AdminTimeframedMetricsMeta;
  metrics: AdminTimeframedMetricsValues;
  showCiData: boolean;
  selectedRange: string;
  onSelectRange: (range: string) => void;
  customStartDate: string;
  customEndDate: string;
  onCustomRangeChange: (startDate: string, endDate: string) => void;
  usageAnalyticsData: AdminUsageAnalyticsData;
}

const AdminTimeframedMetrics = ({
  data,
  metrics,
  showCiData,
  selectedRange,
  onSelectRange,
  customStartDate,
  customEndDate,
  onCustomRangeChange,
  usageAnalyticsData,
}: AdminTimeframedMetricsProps) => {
  return (
    <div className="rounded-2xl border border-border/60 bg-gradient-card p-6 sm:p-8 shadow-sm">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="text-xl sm:text-2xl font-semibold text-foreground">
            Timeframed Metrics
          </h2>
          <p className="text-sm text-muted-foreground">
            Choose a timeframe to review recent activity.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {data.timeframes.map((range) => (
            <button
              key={range}
              className={`rounded-full border px-4 py-2 text-sm font-semibold transition ${
                range === selectedRange
                  ? "border-primary/50 bg-primary/10 text-primary"
                  : "border-border/60 bg-background text-muted-foreground hover:text-foreground"
              }`}
              onClick={() => onSelectRange(range)}
            >
              {range}
            </button>
          ))}
          <button
            className={`rounded-full border px-4 py-2 text-sm font-semibold transition ${
              selectedRange === "Custom"
                ? "border-primary/50 bg-primary/10 text-primary"
                : "border-border/60 bg-background text-muted-foreground hover:text-foreground"
            }`}
            onClick={() => onSelectRange("Custom")}
          >
            Custom
          </button>
          <div
            className={`flex items-center gap-2 rounded-full border border-border/60 bg-background px-3 py-1 text-xs text-muted-foreground transition ${
              selectedRange === "Custom"
                ? "opacity-100"
                : "opacity-0 pointer-events-none"
            }`}
          >
            <span className="text-[11px] uppercase tracking-[0.2em]">Range</span>
            <input
              type="date"
              aria-label="Custom start date"
              className="rounded-full border border-transparent bg-transparent px-2 py-1 text-xs text-foreground focus:border-primary/50 focus:outline-none"
              value={customStartDate}
              onChange={(event) =>
                onCustomRangeChange(event.target.value, customEndDate)
              }
            />
            <span className="text-[11px] uppercase tracking-[0.2em]">to</span>
            <input
              type="date"
              aria-label="Custom end date"
              className="rounded-full border border-transparent bg-transparent px-2 py-1 text-xs text-foreground focus:border-primary/50 focus:outline-none"
              value={customEndDate}
              onChange={(event) =>
                onCustomRangeChange(customStartDate, event.target.value)
              }
            />
          </div>
        </div>
      </div>

      <div className="mt-6 grid gap-4 sm:gap-6 md:grid-cols-2 xl:grid-cols-4">
        <div className="rounded-2xl border border-border/60 bg-background p-5">
          <p className="text-xs font-semibold uppercase tracking-[0.2em] text-muted-foreground">
            Sessions Started
          </p>
          <p className="mt-4 text-3xl font-semibold text-foreground">
            {metrics.sessionsStarted}
          </p>
        </div>
        <div className="rounded-2xl border border-border/60 bg-background p-5">
          <p className="text-xs font-semibold uppercase tracking-[0.2em] text-muted-foreground">
            Active Users
          </p>
          <p className="mt-4 text-3xl font-semibold text-foreground">
            {metrics.activeUsers}
          </p>
        </div>
        {showCiData && (
          <div className="rounded-2xl border border-border/60 bg-background p-5">
            <p className="text-xs font-semibold uppercase tracking-[0.2em] text-muted-foreground">
              CI Runs
            </p>
            <p className="mt-4 text-3xl font-semibold text-foreground">
              {metrics.ciRuns}
            </p>
          </div>
        )}
        <div className="rounded-2xl border border-border/60 bg-background p-5">
          <p className="text-xs font-semibold uppercase tracking-[0.2em] text-muted-foreground">
            Time Saved
          </p>
          <p className="mt-4 text-3xl font-semibold text-foreground">
            {metrics.timeSavedHours}h
          </p>
        </div>
      </div>

      <div className="mt-8">
        <AdminUsageAnalytics
          data={usageAnalyticsData}
          rangeLabel={selectedRange}
        />
      </div>
    </div>
  );
};

export default AdminTimeframedMetrics;
