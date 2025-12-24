// Data contracts for the admin dashboard UI.
// Keep these types stable; backend wiring should map API responses into these shapes.
export type AdminAllTimeMetricsData = {
  clusterName: string;
  operatorVersion: string;
  operatorUpdatedAt: string;
  tier: string;
  mirrordVersion: string;
  devTimeSavedHours: number;
  timeSavedSummary: string;
  avgUserSessionMinutes: number;
  avgCiWaitMinutes: number;
  licensesUsed: number;
  licensesTotal: number;
  activeUserSessions: number;
  activeCiSessions: number;
  totalUserSessions: number;
  totalCiSessions: number;
  maxUserConcurrency: number;
  maxCiConcurrency: number;
  totalSessionTime: string;
};

// Meta for time range selection (UI control state).
export type AdminTimeframedMetricsMeta = {
  selectedRange: string;
  timeframes: string[];
  customStartDate: string;
  customEndDate: string;
};

// Metrics values for a given time range.
export type AdminTimeframedMetricsValues = {
  sessionsStarted: number;
  activeUsers: number;
  ciRuns: number;
  timeSavedHours: number;
};

// Usage analytics (per user) for the current time range.
export type AdminUsageAnalyticsData = {
  title: string;
  users: Array<{ username: string; sessions: number; totalSessionHours: number }>;
};

// Table row for user activity details (license server).
// Primary key can be machineId or userId, depending on the API.
export type AdminUserActivityRow = {
  id: string;
  machineId: string;
  userId: string;
  activeSince: string;
  lastSession: string;
  totalTime: string;
  sessions: number;
  avgDuration: string;
  dailyAvg: string;
};

// Controls how the activity table groups and labels rows.
export type AdminUserActivityMeta = {
  primaryKey:
    | "machine_id"
    | "username"
    | "client_username"
    | "client_hostname";
};

// TODO: Backend wiring: replace with real data source (API/store). Keep shape stable for easy wiring.
export const adminAllTimeMetricsData: AdminAllTimeMetricsData = {
  clusterName: "mirrord-prod-us-east-1",
  operatorVersion: "3.4.1",
  operatorUpdatedAt: "2 days ago",
  tier: "Enterprise",
  mirrordVersion: "3.141.0",
  devTimeSavedHours: 0,
  timeSavedSummary: "0.8h",
  avgUserSessionMinutes: 12,
  avgCiWaitMinutes: 8,
  licensesUsed: 10,
  licensesTotal: 50,
  activeUserSessions: 10,
  activeCiSessions: 0,
  totalUserSessions: 120,
  totalCiSessions: 30,
  maxUserConcurrency: 42,
  maxCiConcurrency: 29,
  totalSessionTime: "2m 2w",
};

// TODO: Replace with real data source (API/store). Keep shape stable for easy wiring.
export const adminTimeframedMetricsMeta: AdminTimeframedMetricsMeta = {
  selectedRange: "Last 30 days",
  timeframes: ["Today", "Last 7 days", "Last 30 days"],
  customStartDate: "",
  customEndDate: "",
};

// TODO: Backend wiring: replace with real data source (API/store). Keep shape stable for easy wiring.
export const adminTimeframedMetricsByRange: Record<
  string,
  AdminTimeframedMetricsValues
> = {
  Today: {
    sessionsStarted: 28,
    activeUsers: 14,
    ciRuns: 6,
    timeSavedHours: 2,
  },
  "Last 7 days": {
    sessionsStarted: 124,
    activeUsers: 42,
    ciRuns: 22,
    timeSavedHours: 8,
  },
  "Last 30 days": {
    sessionsStarted: 214,
    activeUsers: 64,
    ciRuns: 38,
    timeSavedHours: 12,
  },
  Custom: {
    sessionsStarted: 96,
    activeUsers: 30,
    ciRuns: 16,
    timeSavedHours: 5,
  },
};

// TODO: Replace with real data source (API/store). Keep shape stable for easy wiring.
// TODO: Backend wiring: replace with real data source (API/store). Keep shape stable for easy wiring.
export const adminUsageAnalyticsByRange: Record<string, AdminUsageAnalyticsData> =
  {
    Today: {
      title: "Top Active Users",
      users: [
        { username: "alice.dev", sessions: 22, totalSessionHours: 6 },
        { username: "bob.backend", sessions: 18, totalSessionHours: 5 },
        { username: "charlie.frontend", sessions: 16, totalSessionHours: 4 },
        { username: "eve.devops", sessions: 14, totalSessionHours: 4 },
        { username: "grace.data", sessions: 12, totalSessionHours: 3 },
        { username: "diana.qa", sessions: 10, totalSessionHours: 3 },
      ],
    },
    "Last 7 days": {
      title: "Top Active Users",
      users: [
        { username: "alice.dev", sessions: 68, totalSessionHours: 24 },
        { username: "bob.backend", sessions: 62, totalSessionHours: 22 },
        { username: "charlie.frontend", sessions: 58, totalSessionHours: 20 },
        { username: "eve.devops", sessions: 52, totalSessionHours: 18 },
        { username: "grace.data", sessions: 46, totalSessionHours: 16 },
        { username: "diana.qa", sessions: 40, totalSessionHours: 14 },
        { username: "frank.mobile", sessions: 36, totalSessionHours: 12 },
      ],
    },
    "Last 30 days": {
      title: "Top Active Users",
      users: [
        { username: "alice.dev", sessions: 152, totalSessionHours: 94 },
        { username: "bob.backend", sessions: 126, totalSessionHours: 82 },
        { username: "charlie.frontend", sessions: 110, totalSessionHours: 76 },
        { username: "eve.devops", sessions: 108, totalSessionHours: 72 },
        { username: "grace.data", sessions: 86, totalSessionHours: 60 },
        { username: "diana.qa", sessions: 74, totalSessionHours: 48 },
        { username: "frank.mobile", sessions: 58, totalSessionHours: 40 },
        { username: "henry.security", sessions: 50, totalSessionHours: 34 },
      ],
    },
    Custom: {
      title: "Top Active Users",
      users: [
        { username: "alice.dev", sessions: 44, totalSessionHours: 18 },
        { username: "bob.backend", sessions: 38, totalSessionHours: 16 },
        { username: "charlie.frontend", sessions: 34, totalSessionHours: 14 },
        { username: "eve.devops", sessions: 30, totalSessionHours: 12 },
        { username: "grace.data", sessions: 26, totalSessionHours: 10 },
        { username: "diana.qa", sessions: 22, totalSessionHours: 8 },
      ],
    },
  };

// TODO: Backend wiring: replace with license server API data.
// Expected inputs: page, pageSize, searchQuery, range (selectedRange/custom dates).
export const adminUserActivityMeta: AdminUserActivityMeta = {
  primaryKey: "username",
};

// TODO: Backend wiring: replace with license server API data.
// When primaryKey is username, userId is primary and machineId is secondary.
export const adminUserActivityRows: AdminUserActivityRow[] = [
  {
    id: "DEV-001",
    machineId: "DEV-001",
    userId: "alice.dev",
    activeSince: "Mar 15, 2024",
    lastSession: "Dec 4, 2025",
    totalTime: "2h 32m",
    sessions: 99,
    avgDuration: "1m",
    dailyAvg: "0.2",
  },
  {
    id: "DEV-002",
    machineId: "DEV-002",
    userId: "bob.backend",
    activeSince: "Feb 20, 2024",
    lastSession: "Dec 11, 2025",
    totalTime: "2h 7m",
    sessions: 80,
    avgDuration: "1m",
    dailyAvg: "0.1",
  },
  {
    id: "DEV-003",
    machineId: "DEV-003",
    userId: "charlie.frontend",
    activeSince: "Apr 1, 2024",
    lastSession: "Dec 3, 2025",
    totalTime: "1h 49m",
    sessions: 68,
    avgDuration: "1m",
    dailyAvg: "0.1",
  },
  {
    id: "DEV-005",
    machineId: "DEV-005",
    userId: "eve.devops",
    activeSince: "Mar 5, 2024",
    lastSession: "Nov 27, 2025",
    totalTime: "1h 48m",
    sessions: 74,
    avgDuration: "1m",
    dailyAvg: "0.1",
  },
  {
    id: "DEV-007",
    machineId: "DEV-007",
    userId: "grace.data",
    activeSince: "Feb 1, 2024",
    lastSession: "Nov 28, 2025",
    totalTime: "1h 27m",
    sessions: 65,
    avgDuration: "1m",
    dailyAvg: "0.1",
  },
  {
    id: "DEV-004",
    machineId: "DEV-004",
    userId: "diana.qa",
    activeSince: "Jan 10, 2024",
    lastSession: "Nov 28, 2025",
    totalTime: "1h 14m",
    sessions: 64,
    avgDuration: "1m",
    dailyAvg: "0.1",
  },
  {
    id: "DEV-006",
    machineId: "DEV-006",
    userId: "frank.mobile",
    activeSince: "Apr 15, 2024",
    lastSession: "Dec 9, 2025",
    totalTime: "58m",
    sessions: 60,
    avgDuration: "0m",
    dailyAvg: "0.1",
  },
];
