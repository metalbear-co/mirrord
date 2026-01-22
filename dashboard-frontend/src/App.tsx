import { useState, useEffect, useCallback } from 'react';
import { Users, UserCheck, Clock, Activity } from 'lucide-react';
import { Header } from '@/components/Header';
import { MetricCard, LicenseCard, SessionsCard } from '@/components/MetricCard';
import { UsageChart } from '@/components/UsageChart';
import { SessionDistribution } from '@/components/SessionDistribution';
import { TopTargetsChart, UserActivityChart } from '@/components/TopTargetsChart';
import { SessionsTable } from '@/components/SessionsTable';
import { DateRangePicker } from '@/components/DateRangePicker';
import { applyTheme } from '@/lib/theme';
import {
  useDashboardData,
  useDateRange,
  useFilteredData,
  useSessionsTable,
} from '@/hooks/useDashboardData';
import {
  getDaysUntilExpiration,
  getLicenseStatus,
  calculateTimeSaved,
  getTrendIndicator,
} from '@/lib/utils';

type ChartTab = 'usage' | 'distribution' | 'targets' | 'users';

function App() {
  const [isDarkMode, setIsDarkMode] = useState(() => {
    const saved = localStorage.getItem('darkMode');
    return saved ? JSON.parse(saved) : false;
  });

  const toggleDarkMode = useCallback(() => {
    setIsDarkMode((prev: boolean) => {
      const newValue = !prev;
      localStorage.setItem('darkMode', JSON.stringify(newValue));
      return newValue;
    });
  }, []);

  useEffect(() => {
    applyTheme(isDarkMode);
  }, [isDarkMode]);

  const { data, isLoading, isError, error, refresh, lastUpdated } = useDashboardData();
  const { dateRange, setPreset, setCustomRange } = useDateRange();
  const filteredData = useFilteredData(data, dateRange);
  const [activeTab, setActiveTab] = useState<ChartTab>('usage');

  const {
    filteredSessions,
    searchQuery,
    setSearchQuery,
    sort,
    toggleSort,
    expandedSessionId,
    toggleExpanded,
  } = useSessionsTable(filteredData?.sessions || []);

  if (isLoading && !data) {
    return <LoadingState />;
  }

  if (isError && !data) {
    return <ErrorState error={error} onRetry={refresh} />;
  }

  if (!filteredData) {
    return <EmptyState />;
  }

  const { license, statistics, sessions } = filteredData;
  const daysUntilExpiration = getDaysUntilExpiration(license.expire_at);
  const licenseStatus = getLicenseStatus(license.expire_at);
  const timeSaved = calculateTimeSaved(statistics);
  const ciSessions = sessions.filter((s) => s.is_ci).length;
  const userSessions = sessions.length - ciSessions;

  // Calculate trend (comparing last 7 days to previous 7 days)
  const recentUsage = statistics.usage_over_time.slice(-7);
  const previousUsage = statistics.usage_over_time.slice(-14, -7);
  const recentSessions = recentUsage.reduce((sum, d) => sum + d.sessions, 0);
  const previousSessions = previousUsage.reduce((sum, d) => sum + d.sessions, 0);
  const sessionTrend = getTrendIndicator(recentSessions, previousSessions);

  const tabs: Array<{ id: ChartTab; label: string }> = [
    { id: 'usage', label: 'Usage Over Time' },
    { id: 'distribution', label: 'Mode Distribution' },
    { id: 'targets', label: 'Top Targets' },
    { id: 'users', label: 'User Activity' },
  ];

  return (
    <div className="min-h-screen bg-[var(--background)] p-4 sm:p-6 lg:p-8 transition-colors duration-200">
      <div className="max-w-7xl mx-auto">
        <Header
          lastUpdated={lastUpdated}
          onRefresh={refresh}
          isLoading={isLoading}
          isDarkMode={isDarkMode}
          onToggleDarkMode={toggleDarkMode}
        />

        {/* Key Metrics Row */}
        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-5 gap-4 mb-8">
          <LicenseCard
            organization={license.organization}
            daysUntilExpiration={daysUntilExpiration}
            status={licenseStatus}
          />

          <MetricCard
            title="Daily Active Users"
            value={statistics.daily_active_users}
            icon={<Users className="w-5 h-5" />}
            trend={sessionTrend}
          />

          <MetricCard
            title="Monthly Active Users"
            value={statistics.monthly_active_users}
            subtitle={`${statistics.total_users} total users`}
            icon={<UserCheck className="w-5 h-5" />}
          />

          <SessionsCard
            activeSessions={sessions.length}
            userSessions={userSessions}
            ciSessions={ciSessions}
          />

          <MetricCard
            title="Time Saved"
            value={`${timeSaved}h`}
            subtitle="Estimated developer hours"
            icon={<Clock className="w-5 h-5" />}
            status="success"
          />
        </div>

        {/* Date Range Picker */}
        <div className="mb-6">
          <DateRangePicker
            dateRange={dateRange}
            onPresetChange={setPreset}
            onCustomRangeChange={setCustomRange}
          />
        </div>

        {/* Charts Section with Tabs */}
        <div className="mb-8">
          <div className="flex flex-wrap gap-2 mb-4">
            {tabs.map((tab) => (
              <button
                key={tab.id}
                onClick={() => setActiveTab(tab.id)}
                className={`px-4 py-2 rounded-lg text-sm font-medium transition-all ${
                  activeTab === tab.id
                    ? 'bg-primary text-white shadow-[-7px_6.5px_0px_rgba(0,0,0,1)]'
                    : 'bg-[var(--card)] text-[var(--foreground)] hover:shadow-[-7px_6.5px_0px_rgba(0,0,0,1)] border border-[var(--border)]'
                }`}
              >
                {tab.label}
              </button>
            ))}
          </div>

          <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
            {activeTab === 'usage' && (
              <>
                <UsageChart data={statistics.usage_over_time} isDarkMode={isDarkMode} />
                <SessionDistribution
                  mirrorCount={statistics.sessions_by_mode.mirror}
                  stealCount={statistics.sessions_by_mode.steal}
                  isDarkMode={isDarkMode}
                />
              </>
            )}
            {activeTab === 'distribution' && (
              <>
                <SessionDistribution
                  mirrorCount={statistics.sessions_by_mode.mirror}
                  stealCount={statistics.sessions_by_mode.steal}
                  isDarkMode={isDarkMode}
                />
                <div className="card flex items-center justify-center">
                  <div className="text-center">
                    <Activity className="w-12 h-12 text-primary mx-auto mb-4" />
                    <p className="text-2xl font-bold text-[var(--foreground)]">
                      {statistics.sessions_by_mode.steal + statistics.sessions_by_mode.mirror}
                    </p>
                    <p className="text-muted">Total Sessions</p>
                    <p className="text-sm text-[var(--muted-foreground)] mt-2">
                      {((statistics.sessions_by_mode.steal /
                        (statistics.sessions_by_mode.steal + statistics.sessions_by_mode.mirror)) *
                        100).toFixed(0)}
                      % using steal mode
                    </p>
                  </div>
                </div>
              </>
            )}
            {activeTab === 'targets' && (
              <>
                <TopTargetsChart sessionsByTarget={statistics.sessions_by_target} isDarkMode={isDarkMode} />
                <div className="card">
                  <h3 className="text-lg font-semibold text-[var(--foreground)] mb-4">Target Details</h3>
                  <div className="space-y-3 max-h-[280px] overflow-y-auto">
                    {Object.entries(statistics.sessions_by_target)
                      .sort(([, a], [, b]) => b - a)
                      .map(([target, count]) => (
                        <div
                          key={target}
                          className="flex items-center justify-between p-3 bg-[var(--muted)]/30 rounded-lg"
                        >
                          <span className="text-[var(--foreground)]">{target}</span>
                          <span className="text-primary font-medium">{count} sessions</span>
                        </div>
                      ))}
                  </div>
                </div>
              </>
            )}
            {activeTab === 'users' && (
              <>
                <UserActivityChart sessionsByUser={statistics.sessions_by_user} isDarkMode={isDarkMode} />
                <div className="card">
                  <h3 className="text-lg font-semibold text-[var(--foreground)] mb-4">User Details</h3>
                  <div className="space-y-3 max-h-[280px] overflow-y-auto">
                    {Object.entries(statistics.sessions_by_user)
                      .sort(([, a], [, b]) => b - a)
                      .map(([user, count]) => (
                        <div
                          key={user}
                          className="flex items-center justify-between p-3 bg-[var(--muted)]/30 rounded-lg"
                        >
                          <span className="text-[var(--foreground)]">{user}</span>
                          <span className="text-primary font-medium">{count} sessions</span>
                        </div>
                      ))}
                  </div>
                </div>
              </>
            )}
          </div>
        </div>

        {/* Active Sessions Table */}
        <SessionsTable
          sessions={filteredSessions}
          searchQuery={searchQuery}
          onSearchChange={setSearchQuery}
          sort={sort}
          onSort={toggleSort}
          expandedSessionId={expandedSessionId}
          onToggleExpand={toggleExpanded}
        />
      </div>
    </div>
  );
}

function LoadingState() {
  return (
    <div className="min-h-screen bg-[var(--background)] flex items-center justify-center">
      <div className="text-center">
        <div className="w-16 h-16 border-4 border-primary border-t-transparent rounded-full animate-spin mx-auto mb-4"></div>
        <p className="text-[var(--foreground)] text-lg">Loading dashboard...</p>
        <p className="text-[var(--muted-foreground)] text-sm mt-2">Connecting to mirrord operator</p>
      </div>
    </div>
  );
}

function ErrorState({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  return (
    <div className="min-h-screen bg-[var(--background)] flex items-center justify-center p-4">
      <div className="text-center max-w-md">
        <div className="w-16 h-16 bg-destructive/20 rounded-full flex items-center justify-center mx-auto mb-4">
          <svg className="w-8 h-8 text-destructive" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path
              strokeLinecap="round"
              strokeLinejoin="round"
              strokeWidth={2}
              d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z"
            />
          </svg>
        </div>
        <h2 className="text-xl font-bold text-[var(--foreground)] mb-2">Failed to Load Dashboard</h2>
        <p className="text-[var(--muted-foreground)] mb-4">
          {error instanceof Error ? error.message : 'Unable to connect to the mirrord operator'}
        </p>
        <p className="text-[var(--muted-foreground)] text-sm mb-6">
          Make sure kubectl proxy is running: <code className="bg-[var(--muted)] px-2 py-1 rounded">kubectl proxy</code>
        </p>
        <button onClick={onRetry} className="btn-primary">
          Try Again
        </button>
      </div>
    </div>
  );
}

function EmptyState() {
  return (
    <div className="min-h-screen bg-[var(--background)] flex items-center justify-center p-4">
      <div className="text-center max-w-md">
        <div className="w-16 h-16 bg-primary/20 rounded-full flex items-center justify-center mx-auto mb-4">
          <Activity className="w-8 h-8 text-primary" />
        </div>
        <h2 className="text-xl font-bold text-[var(--foreground)] mb-2">No Data Available</h2>
        <p className="text-[var(--muted-foreground)]">
          No mirrord usage data found. Start using mirrord to see your team's activity here.
        </p>
      </div>
    </div>
  );
}

export default App;
