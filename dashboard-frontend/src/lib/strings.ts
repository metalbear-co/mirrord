/**
 * Centralized strings for the dashboard.
 * This file makes it easy to:
 * 1. Find and update text across the app
 * 2. Add internationalization (i18n) support in the future
 */

export const strings = {
  // App-level
  app: {
    title: 'mirrord',
    subtitle: 'Utilization',
  },

  // Toast messages
  toast: {
    syncSuccess: 'Dashboard synced',
    syncError: 'Failed to sync',
    syncErrorDescription: 'Check your connection and try again',
  },

  // Metric cards
  metrics: {
    license: {
      title: 'License',
      tooltip: 'Your mirrord license tier',
      tiers: {
        free: 'Free',
        team: 'Team',
        enterprise: 'Enterprise',
      },
      expired: 'expired',
      expiresIn: (days: number) => `expires in ${days}d`,
    },
    dau: {
      title: 'DAU',
      subtitle: 'users today',
      tooltip: 'Daily active users',
    },
    mau: {
      title: 'MAU',
      subtitleSuffix: 'total',
      tooltip: 'Monthly active users',
    },
    sessions: {
      title: 'Sessions',
      subtitle: 'active now',
      tooltip: 'Currently running mirrord sessions',
    },
    timeSaved: {
      title: 'Time Saved',
      subtitle: 'dev hours saved',
      tooltip: 'Estimated hours saved',
    },
  },

  // Chart tabs
  charts: {
    tabs: {
      usage: 'Usage Over Time',
      distribution: 'Mode Distribution',
      targets: 'Top Targets',
      users: 'User Activity',
    },
    layout: {
      switchToFullWidth: 'Switch to full-width view',
      switchToGrid: 'Switch to grid view',
    },
    usageChart: {
      title: 'Usage Over Time',
      sessions: 'Sessions',
      activeUsers: 'Active Users',
    },
    distribution: {
      title: 'Session Mode Distribution',
      steal: 'Steal',
      mirror: 'Mirror',
      stealSessions: 'Steal Sessions',
      mirrorSessions: 'Mirror Sessions',
      totalSessions: 'Total Sessions',
      usingStealMode: '% using steal mode',
    },
    topTargets: {
      title: 'Top Targets',
      detailsTitle: 'Target Details',
      sessionsLabel: 'sessions',
    },
    userActivity: {
      title: 'User Activity',
      detailsTitle: 'User Details',
      sessionsLabel: 'sessions',
    },
  },

  // Sessions table
  table: {
    title: 'Active Sessions',
    searchPlaceholder: 'Search sessions...',
    columns: {
      user: 'User',
      target: 'Target',
      namespace: 'Namespace',
      mode: 'Mode',
      duration: 'Duration',
      started: 'Started',
    },
    details: {
      sessionId: 'Session ID',
      ports: 'Ports',
      startedAt: 'Started At',
      type: 'Type',
    },
    sessionTypes: {
      ci: 'CI Pipeline',
      user: 'User Session',
    },
    empty: 'No active sessions',
    pagination: {
      rowsPerPage: 'Rows per page:',
      of: 'of',
    },
    aria: {
      clearSearch: 'Clear search',
      firstPage: 'First page',
      previousPage: 'Previous page',
      nextPage: 'Next page',
      lastPage: 'Last page',
    },
  },

  // Date range picker
  dateRange: {
    label: 'Date Range:',
    presets: {
      '7d': '7 Days',
      '30d': '30 Days',
      '90d': '90 Days',
    },
    custom: 'Custom',
    apply: 'Apply',
  },

  // Header / AppBar
  header: {
    lastUpdated: 'Last updated:',
    refresh: 'Sync',
    refreshing: 'Syncing...',
    lightMode: 'Switch to light mode',
    darkMode: 'Switch to dark mode',
    docs: 'Docs',
    console: 'Console',
    github: 'GitHub',
  },

  // Loading state
  loading: {
    title: 'Loading...',
  },

  // Error state
  error: {
    title: 'Failed to Load Dashboard',
    defaultMessage: 'Unable to connect to the mirrord operator',
    proxyHint: 'Make sure kubectl proxy is running:',
    proxyCommand: 'kubectl proxy',
    retry: 'Try Again',
  },

  // Empty state
  empty: {
    title: 'No Data Available',
    message: "No mirrord usage data found. Start using mirrord to see your team's activity here.",
    waiting: 'Waiting for first session...',
  },
} as const;

// Type helper for accessing nested string values
export type Strings = typeof strings;
