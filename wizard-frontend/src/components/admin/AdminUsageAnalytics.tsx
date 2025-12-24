import { useState } from "react";
import type { AdminUsageAnalyticsData } from "./adminMetricsData";

interface AdminUsageAnalyticsProps {
  data: AdminUsageAnalyticsData;
  rangeLabel: string;
}

const AdminUsageAnalytics = ({ data, rangeLabel }: AdminUsageAnalyticsProps) => {
  const [showTotalTime, setShowTotalTime] = useState(false);

  const sortedUsers = [...data.users].sort((a, b) => {
    const aValue = showTotalTime ? a.totalSessionHours : a.sessions;
    const bValue = showTotalTime ? b.totalSessionHours : b.sessions;
    return bValue - aValue;
  });

  const maxValue = Math.max(
    1,
    ...sortedUsers.map((user) =>
      showTotalTime ? user.totalSessionHours : user.sessions,
    ),
  );

  return (
    <div className="space-y-4">
      <h2 className="text-xl sm:text-2xl font-semibold text-foreground">
        Usage Analytics
      </h2>
      <div className="rounded-2xl border border-border/60 bg-gradient-card p-6 sm:p-8 shadow-sm">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <h3 className="text-lg sm:text-2xl font-semibold text-foreground">
              {data.title}
            </h3>
            <p className="mt-1 text-sm text-muted-foreground">
              Range: {rangeLabel}
            </p>
          </div>
          <div className="inline-flex rounded-full border border-border/60 bg-background p-1 shadow-sm">
            <button
              className={`rounded-full px-4 py-2 text-sm font-semibold transition ${
                showTotalTime
                  ? "bg-primary text-primary-foreground shadow-glow"
                  : "text-muted-foreground hover:text-foreground"
              }`}
              onClick={() => setShowTotalTime(true)}
            >
              By Time
            </button>
            <button
              className={`rounded-full px-4 py-2 text-sm font-semibold transition ${
                showTotalTime
                  ? "text-muted-foreground hover:text-foreground"
                  : "bg-primary text-primary-foreground shadow-glow"
              }`}
              onClick={() => setShowTotalTime(false)}
            >
              By Count
            </button>
          </div>
        </div>
        <div className="mt-6">
          <div className="relative rounded-2xl bg-background/70 p-6">
            <div className="absolute inset-y-6 left-32 right-6 grid grid-cols-4 gap-6">
              <div className="border-l border-dashed border-border/60" />
              <div className="border-l border-dashed border-border/60" />
              <div className="border-l border-dashed border-border/60" />
              <div className="border-l border-dashed border-border/60" />
            </div>
            <div className="relative space-y-4">
              {sortedUsers.map((user) => (
                <div
                  key={user.username}
                  className="grid items-center gap-4"
                  style={{ gridTemplateColumns: "8rem 1fr" }}
                >
                  <div className="text-sm font-semibold text-muted-foreground">
                    {user.username}
                  </div>
                  <div className="flex items-center gap-3">
                    <div className="relative h-10 w-full rounded-full bg-background">
                      <div
                        className="absolute inset-y-0 left-0 rounded-full bg-primary/80 shadow-sm"
                        style={{
                          width: `${Math.max(
                            6,
                            Math.round(
                              ((showTotalTime
                                ? user.totalSessionHours
                                : user.sessions) /
                                maxValue) *
                                100,
                            ),
                          )}%`,
                        }}
                      />
                    </div>
                    <div className="w-12 text-right text-sm font-semibold text-foreground">
                      {showTotalTime
                        ? `${user.totalSessionHours}h`
                        : user.sessions}
                    </div>
                  </div>
                </div>
              ))}
            </div>
            <div className="mt-6 flex items-center justify-between pl-32 text-xs text-muted-foreground">
              <span>0</span>
              <span>
                {showTotalTime ? `${maxValue} hours` : `${maxValue} sessions`}
              </span>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
};

export default AdminUsageAnalytics;
