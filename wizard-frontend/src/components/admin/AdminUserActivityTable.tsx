import type { AdminUserActivityRow } from "./adminMetricsData";
import { Input } from "../ui/input";
import { Button } from "../ui/button";

interface AdminUserActivityTableProps {
  rows: AdminUserActivityRow[];
  primaryKey: "machine_id" | "username" | "client_username" | "client_hostname";
  page: number;
  totalPages: number;
  rangeLabel: string;
  searchQuery: string;
  sortKey: AdminUserActivitySortKey;
  sortDirection: "asc" | "desc";
  // Backend wiring: use searchQuery + rangeLabel to query license server.
  onSearchChange: (value: string) => void;
  onPageChange: (page: number) => void;
  // Backend wiring: export should call server for CSV, not client-side.
  onExport: () => void;
  onSortChange: (key: AdminUserActivitySortKey) => void;
}

export type AdminUserActivitySortKey =
  | "machineId"
  | "userId"
  | "activeSince"
  | "lastSession"
  | "totalTime"
  | "sessions"
  | "avgDuration"
  | "dailyAvg";

const AdminUserActivityTable = ({
  rows,
  primaryKey,
  page,
  totalPages,
  rangeLabel,
  searchQuery,
  sortKey,
  sortDirection,
  onSearchChange,
  onPageChange,
  onExport,
  onSortChange,
}: AdminUserActivityTableProps) => {
  const headerButtonClass =
    "flex items-center gap-2 font-semibold text-muted-foreground hover:text-foreground";
  const sortBadge = (key: AdminUserActivitySortKey) =>
    sortKey === key ? (sortDirection === "asc" ? "asc" : "desc") : "";
  const primaryHeaderLabel =
    primaryKey === "machine_id" ? "Machine ID / User" : "User / Machine ID";

  return (
    <div className="rounded-2xl border border-border/60 bg-gradient-card p-6 sm:p-8 shadow-sm">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h3 className="text-lg sm:text-2xl font-semibold text-foreground">
            User Activity Details
          </h3>
          <p className="mt-1 text-sm text-muted-foreground">
            Range: {rangeLabel}
          </p>
        </div>
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
          <Input
            className="min-w-[220px] bg-background"
            placeholder="Search users..."
            value={searchQuery}
            onChange={(event) => onSearchChange(event.target.value)}
          />
          <Button variant="outline" onClick={onExport}>
            Export CSV
          </Button>
        </div>
      </div>

      <div className="mt-6 overflow-hidden rounded-2xl border border-border/60 bg-background/80">
        <table className="min-w-full text-sm">
          <thead className="bg-muted text-muted-foreground">
            <tr>
              <th className="px-4 py-3 text-left">
                <button
                  className={headerButtonClass}
                  onClick={() => onSortChange("machineId")}
                >
                  {primaryHeaderLabel}
                  <span className="text-xs text-muted-foreground">
                    {sortBadge("machineId")}
                  </span>
                </button>
              </th>
              <th className="px-4 py-3 text-left">
                <button
                  className={headerButtonClass}
                  onClick={() => onSortChange("activeSince")}
                >
                  Active Since
                  <span className="text-xs text-muted-foreground">
                    {sortBadge("activeSince")}
                  </span>
                </button>
              </th>
              <th className="px-4 py-3 text-left">
                <button
                  className={headerButtonClass}
                  onClick={() => onSortChange("lastSession")}
                >
                  Last Session
                  <span className="text-xs text-muted-foreground">
                    {sortBadge("lastSession")}
                  </span>
                </button>
              </th>
              <th className="px-4 py-3 text-left">
                <button
                  className={headerButtonClass}
                  onClick={() => onSortChange("totalTime")}
                >
                  Total Time
                  <span className="text-xs text-muted-foreground">
                    {sortBadge("totalTime")}
                  </span>
                </button>
              </th>
              <th className="px-4 py-3 text-left">
                <button
                  className={headerButtonClass}
                  onClick={() => onSortChange("sessions")}
                >
                  Sessions
                  <span className="text-xs text-muted-foreground">
                    {sortBadge("sessions")}
                  </span>
                </button>
              </th>
              <th className="px-4 py-3 text-left">
                <button
                  className={headerButtonClass}
                  onClick={() => onSortChange("avgDuration")}
                >
                  Avg Duration
                  <span className="text-xs text-muted-foreground">
                    {sortBadge("avgDuration")}
                  </span>
                </button>
              </th>
              <th className="px-4 py-3 text-left">
                <button
                  className={headerButtonClass}
                  onClick={() => onSortChange("dailyAvg")}
                >
                  Daily Avg
                  <span className="text-xs text-muted-foreground">
                    {sortBadge("dailyAvg")}
                  </span>
                </button>
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={row.id} className="border-t border-border/60">
                <td className="px-4 py-4 font-semibold text-foreground">
                  {primaryKey === "machine_id" ? (
                    <>
                      <div>{row.machineId}</div>
                      <div className="text-xs font-medium text-muted-foreground">
                        {row.userId}
                      </div>
                    </>
                  ) : (
                    <>
                      <div>{row.userId}</div>
                      <div className="text-xs font-medium text-muted-foreground">
                        {row.machineId}
                      </div>
                    </>
                  )}
                </td>
                <td className="px-4 py-4">{row.activeSince}</td>
                <td className="px-4 py-4">{row.lastSession}</td>
                <td className="px-4 py-4 font-semibold text-foreground">
                  {row.totalTime}
                </td>
                <td className="px-4 py-4">{row.sessions}</td>
                <td className="px-4 py-4">{row.avgDuration}</td>
                <td className="px-4 py-4">{row.dailyAvg}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <div className="mt-4 flex items-center justify-between text-sm text-muted-foreground">
        <span>
          Page {page} of {totalPages}
        </span>
        <div className="flex items-center gap-2">
          <button
            className="rounded-full border border-border/60 px-3 py-1 text-xs font-semibold text-foreground disabled:opacity-40"
            onClick={() => onPageChange(page - 1)}
            disabled={page <= 1}
          >
            Previous
          </button>
          <button
            className="rounded-full border border-border/60 px-3 py-1 text-xs font-semibold text-foreground disabled:opacity-40"
            onClick={() => onPageChange(page + 1)}
            disabled={page >= totalPages}
          >
            Next
          </button>
        </div>
      </div>
    </div>
  );
};

export default AdminUserActivityTable;
