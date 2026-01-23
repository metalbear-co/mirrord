import { useState, useEffect, useCallback } from 'react';
import { Users, UserCheck, Clock, Activity, LayoutGrid, Maximize2 } from 'lucide-react';
import { AppBar } from '@/components/AppBar';
import { Toaster } from '@/components/Toaster';
import { MetricCard, LicenseCard, SessionsCard } from '@/components/MetricCard';
import { useToast } from '@/hooks/useToast';
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
  const [chartLayout, setChartLayout] = useState<'grid' | 'full'>(() => {
    const saved = localStorage.getItem('chartLayout');
    return (saved === 'full' || saved === 'grid') ? saved : 'grid';
  });
  const { toasts, success, error: toastError, dismiss } = useToast();

  const toggleChartLayout = useCallback(() => {
    setChartLayout((prev) => {
      const newValue = prev === 'grid' ? 'full' : 'grid';
      localStorage.setItem('chartLayout', newValue);
      return newValue;
    });
  }, []);

  const handleRefresh = useCallback(async () => {
    try {
      await refresh();
      success('Dashboard synced');
    } catch {
      toastError('Failed to sync', 'Check your connection and try again');
    }
  }, [refresh, success, toastError]);

  const {
    filteredSessions,
    totalSessions,
    searchQuery,
    setSearchQuery,
    sort,
    toggleSort,
    expandedSessionId,
    toggleExpanded,
    currentPage,
    totalPages,
    pageSize,
    onPageChange,
    onPageSizeChange,
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
    <div className="min-h-screen bg-[var(--background)] transition-colors duration-200">
      <AppBar
        isDarkMode={isDarkMode}
        onToggleDarkMode={toggleDarkMode}
        lastUpdated={lastUpdated}
        onRefresh={handleRefresh}
        isLoading={isLoading}
      />
      <Toaster toasts={toasts} onDismiss={dismiss} />
      <div className="p-4 sm:p-6 lg:p-8">
        <div className="max-w-7xl mx-auto">

        {/* Key Metrics Row */}
        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-5 gap-4 mb-8">
          <LicenseCard
            organization={license.organization}
            daysUntilExpiration={daysUntilExpiration}
            status={licenseStatus}
            className="animate-card-enter animate-card-enter-1"
          />

          <MetricCard
            title="Daily Active Users"
            value={statistics.daily_active_users}
            icon={<Users className="w-5 h-5" />}
            trend={sessionTrend}
            className="animate-card-enter animate-card-enter-2"
            tooltip="Unique users who ran mirrord today"
          />

          <MetricCard
            title="Monthly Active Users"
            value={statistics.monthly_active_users}
            subtitle={`${statistics.total_users} total users`}
            icon={<UserCheck className="w-5 h-5" />}
            className="animate-card-enter animate-card-enter-3"
            tooltip="Unique users who ran mirrord in the last 30 days"
          />

          <SessionsCard
            activeSessions={sessions.length}
            userSessions={userSessions}
            ciSessions={ciSessions}
            className="animate-card-enter animate-card-enter-4"
          />

          <MetricCard
            title="Time Saved"
            value={`${timeSaved}h`}
            subtitle="Estimated developer hours"
            icon={<Clock className="w-5 h-5" />}
            className="animate-card-enter animate-card-enter-5"
            tooltip="Estimated hours saved vs traditional remote debugging workflows"
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
          <div className="flex flex-wrap items-center justify-between gap-4 mb-4">
            <div className="flex flex-wrap gap-2">
              {tabs.map((tab) => (
                <button
                  key={tab.id}
                  onClick={() => setActiveTab(tab.id)}
                  className={`px-4 py-2 rounded-lg text-body-sm font-medium transition-all active:scale-[0.98] ${
                    activeTab === tab.id
                      ? 'bg-primary text-white shadow-[-7px_6.5px_0px_rgba(0,0,0,1)] active:shadow-none'
                      : 'bg-[var(--card)] text-[var(--foreground)] hover:shadow-[-7px_6.5px_0px_rgba(0,0,0,1)] active:shadow-none border border-[var(--border)]'
                  }`}
                >
                  {tab.label}
                </button>
              ))}
            </div>
            <button
              onClick={toggleChartLayout}
              className="p-2 rounded-lg bg-[var(--card)] border border-[var(--border)] text-[var(--muted-foreground)] hover:text-[var(--foreground)] hover:shadow-[-7px_6.5px_0px_rgba(0,0,0,1)] active:scale-[0.98] active:shadow-none transition-all"
              title={chartLayout === 'grid' ? 'Switch to full-width view' : 'Switch to grid view'}
            >
              {chartLayout === 'grid' ? <Maximize2 className="w-4 h-4" /> : <LayoutGrid className="w-4 h-4" />}
            </button>
          </div>

          <div key={activeTab} className={`grid gap-6 animate-fade-in ${chartLayout === 'full' ? 'grid-cols-1' : 'grid-cols-1 lg:grid-cols-2'}`}>
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
                    <p className="text-h4 font-bold text-[var(--foreground)]">
                      {statistics.sessions_by_mode.steal + statistics.sessions_by_mode.mirror}
                    </p>
                    <p className="text-muted">Total Sessions</p>
                    <p className="text-body-sm text-[var(--muted-foreground)] mt-2">
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
                  <h3 className="text-h4 font-semibold text-[var(--foreground)] mb-4">Target Details</h3>
                  <div className="space-y-3 max-h-[280px] overflow-y-auto">
                    {Object.entries(statistics.sessions_by_target)
                      .sort(([, a], [, b]) => b - a)
                      .map(([target, count]) => (
                        <div
                          key={target}
                          className="flex items-center justify-between p-3 bg-[var(--muted)]/30 rounded-lg text-body-sm"
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
                  <h3 className="text-h4 font-semibold text-[var(--foreground)] mb-4">User Details</h3>
                  <div className="space-y-3 max-h-[280px] overflow-y-auto">
                    {Object.entries(statistics.sessions_by_user)
                      .sort(([, a], [, b]) => b - a)
                      .map(([user, count]) => (
                        <div
                          key={user}
                          className="flex items-center justify-between p-3 bg-[var(--muted)]/30 rounded-lg text-body-sm"
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
          totalSessions={totalSessions}
          searchQuery={searchQuery}
          onSearchChange={setSearchQuery}
          sort={sort}
          onSort={toggleSort}
          expandedSessionId={expandedSessionId}
          onToggleExpand={toggleExpanded}
          currentPage={currentPage}
          totalPages={totalPages}
          pageSize={pageSize}
          onPageChange={onPageChange}
          onPageSizeChange={onPageSizeChange}
        />
        </div>
      </div>
    </div>
  );
}

function LoadingState() {
  return (
    <div className="min-h-screen bg-[var(--background)] p-4 sm:p-6 lg:p-8">
      <div className="max-w-7xl mx-auto">
        {/* Header skeleton */}
        <div className="flex flex-col sm:flex-row items-start sm:items-center justify-between gap-4 mb-8">
          <div className="flex items-center gap-4">
            <div className="w-12 h-12 bg-[var(--muted)] rounded-lg animate-pulse"></div>
            <div>
              <div className="h-7 w-48 bg-[var(--muted)] rounded animate-pulse mb-2"></div>
              <div className="h-4 w-64 bg-[var(--muted)] rounded animate-pulse"></div>
            </div>
          </div>
          <div className="flex items-center gap-3">
            <div className="h-10 w-10 bg-[var(--muted)] rounded-lg animate-pulse"></div>
            <div className="h-10 w-24 bg-[var(--muted)] rounded-lg animate-pulse"></div>
          </div>
        </div>

        {/* Metrics skeleton */}
        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-5 gap-4 mb-8">
          {[...Array(5)].map((_, i) => (
            <div key={i} className="card">
              <div className="flex items-center gap-3 mb-4">
                <div className="w-10 h-10 bg-[var(--muted)] rounded-lg animate-pulse"></div>
                <div className="h-4 w-24 bg-[var(--muted)] rounded animate-pulse"></div>
              </div>
              <div className="h-8 w-16 bg-[var(--muted)] rounded animate-pulse"></div>
            </div>
          ))}
        </div>

        {/* Charts skeleton */}
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6 mb-8">
          {[...Array(2)].map((_, i) => (
            <div key={i} className="card">
              <div className="h-5 w-32 bg-[var(--muted)] rounded animate-pulse mb-4"></div>
              <div className="h-[300px] bg-[var(--muted)]/50 rounded-lg animate-pulse"></div>
            </div>
          ))}
        </div>

        {/* Table skeleton */}
        <div className="card">
          <div className="h-5 w-32 bg-[var(--muted)] rounded animate-pulse mb-4"></div>
          <div className="space-y-3">
            {[...Array(5)].map((_, i) => (
              <div key={i} className="h-12 bg-[var(--muted)]/50 rounded animate-pulse"></div>
            ))}
          </div>
        </div>
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
        <h2 className="text-h4 font-bold text-[var(--foreground)] mb-2">Failed to Load Dashboard</h2>
        <p className="text-[var(--muted-foreground)] mb-4">
          {error instanceof Error ? error.message : 'Unable to connect to the mirrord operator'}
        </p>
        <p className="text-[var(--muted-foreground)] text-body-sm mb-6">
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
        {/* Empty state illustration */}
        <div className="relative w-32 h-32 mx-auto mb-6">
          <div className="absolute inset-0 bg-primary/10 rounded-full"></div>
          <div className="absolute inset-4 bg-primary/20 rounded-full"></div>
          <div className="absolute inset-0 flex items-center justify-center">
            <svg className="w-16 h-16 text-primary" fill="none" viewBox="0 0 24 24" stroke="currentColor">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M9 19v-6a2 2 0 00-2-2H5a2 2 0 00-2 2v6a2 2 0 002 2h2a2 2 0 002-2zm0 0V9a2 2 0 012-2h2a2 2 0 012 2v10m-6 0a2 2 0 002 2h2a2 2 0 002-2m0 0V5a2 2 0 012-2h2a2 2 0 012 2v14a2 2 0 01-2 2h-2a2 2 0 01-2-2z" />
            </svg>
          </div>
          {/* Decorative dots */}
          <div className="absolute -top-2 -right-2 w-4 h-4 bg-secondary rounded-full"></div>
          <div className="absolute -bottom-1 -left-1 w-3 h-3 bg-primary/60 rounded-full"></div>
        </div>
        <h2 className="text-h4 font-bold text-[var(--foreground)] mb-2">No Data Available</h2>
        <p className="text-[var(--muted-foreground)] mb-6">
          No mirrord usage data found. Start using mirrord to see your team's activity here.
        </p>
        <div className="flex items-center justify-center gap-2 text-body-sm text-[var(--muted-foreground)]">
          <Activity className="w-4 h-4" />
          <span>Waiting for first session...</span>
        </div>
      </div>
    </div>
  );
}

export default App;
