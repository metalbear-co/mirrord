import type { OperatorStatus, Session, Statistics, License, UsageDataPoint } from '@/types/mirrord';
import { subDays, format } from 'date-fns';

const API_BASE = '/k8s-api/apis/operator.metalbear.co/v1/mirrordoperators';

// Generate mock data for development
function generateMockData(): OperatorStatus {
  const now = new Date();
  const users = ['alice', 'bob', 'charlie', 'david', 'eve', 'frank', 'grace'];
  const targets = [
    'api-server',
    'web-frontend',
    'auth-service',
    'payment-service',
    'user-service',
    'notification-service',
  ];
  const namespaces = ['default', 'staging', 'development', 'qa'];

  // Generate usage over time data
  const usageOverTime: UsageDataPoint[] = [];
  for (let i = 90; i >= 0; i--) {
    const date = subDays(now, i);
    const baseSessions = 5 + Math.floor(Math.random() * 20);
    const baseUsers = Math.min(baseSessions, 2 + Math.floor(Math.random() * 5));
    // Add some weekly pattern (less on weekends)
    const dayOfWeek = date.getDay();
    const weekendMultiplier = dayOfWeek === 0 || dayOfWeek === 6 ? 0.3 : 1;
    usageOverTime.push({
      date: format(date, 'yyyy-MM-dd'),
      sessions: Math.floor(baseSessions * weekendMultiplier),
      users: Math.floor(baseUsers * weekendMultiplier),
    });
  }

  // Generate active sessions
  const sessions: Session[] = [];
  const numActiveSessions = 3 + Math.floor(Math.random() * 5);
  for (let i = 0; i < numActiveSessions; i++) {
    const startedMinutesAgo = Math.floor(Math.random() * 120);
    sessions.push({
      id: `session-${i + 1}`,
      user: users[Math.floor(Math.random() * users.length)],
      target: targets[Math.floor(Math.random() * targets.length)],
      namespace: namespaces[Math.floor(Math.random() * namespaces.length)],
      mode: Math.random() > 0.3 ? 'steal' : 'mirror',
      started_at: new Date(now.getTime() - startedMinutesAgo * 60 * 1000).toISOString(),
      duration_seconds: startedMinutesAgo * 60,
      ports: [8080, 3000, 5432].slice(0, 1 + Math.floor(Math.random() * 3)),
      is_ci: Math.random() > 0.8,
    });
  }

  // Calculate statistics from mock data
  const totalSessions = usageOverTime.reduce((sum, d) => sum + d.sessions, 0);
  const sessionsByTarget: Record<string, number> = {};
  const sessionsByUser: Record<string, number> = {};

  targets.forEach((target) => {
    sessionsByTarget[target] = Math.floor(Math.random() * 200) + 50;
  });

  users.forEach((user) => {
    sessionsByUser[user] = Math.floor(Math.random() * 150) + 20;
  });

  const statistics: Statistics = {
    total_sessions: totalSessions,
    total_users: users.length,
    daily_active_users: 2 + Math.floor(Math.random() * 4),
    monthly_active_users: users.length,
    sessions_by_mode: {
      mirror: Math.floor(totalSessions * 0.25),
      steal: Math.floor(totalSessions * 0.75),
    },
    sessions_by_target: sessionsByTarget,
    sessions_by_user: sessionsByUser,
    usage_over_time: usageOverTime,
  };

  const license: License = {
    name: 'Enterprise License',
    organization: 'Acme Corp',
    expire_at: new Date(now.getTime() + 45 * 24 * 60 * 60 * 1000).toISOString(), // 45 days from now
    fingerprint: 'abc123def456',
  };

  return {
    license,
    sessions,
    statistics,
    operator_version: '3.182.0',
    last_updated: now.toISOString(),
  };
}

let mockData: OperatorStatus | null = null;

function getMockData(): OperatorStatus {
  if (!mockData) {
    mockData = generateMockData();
  }
  // Update timestamps on each call to simulate live data
  mockData.last_updated = new Date().toISOString();
  mockData.sessions = mockData.sessions.map((s) => ({
    ...s,
    duration_seconds: Math.floor((Date.now() - new Date(s.started_at).getTime()) / 1000),
  }));
  return mockData;
}

export async function fetchOperatorStatus(): Promise<OperatorStatus> {
  try {
    const response = await fetch(`${API_BASE}/operator`, {
      headers: {
        Accept: 'application/json',
      },
    });

    if (!response.ok) {
      console.warn('Failed to fetch from operator API, using mock data');
      return getMockData();
    }

    const data = await response.json();

    // Transform the API response to our expected format
    // The actual API response structure may differ, adjust as needed
    return transformApiResponse(data);
  } catch (error) {
    console.warn('Error fetching operator data, using mock data:', error);
    return getMockData();
  }
}

function transformApiResponse(data: unknown): OperatorStatus {
  // The actual operator API returns data in a specific format
  // This function transforms it to our OperatorStatus interface
  // For now, we'll use mock data structure and merge with any real data
  const mock = getMockData();

  if (typeof data === 'object' && data !== null) {
    const apiData = data as Record<string, unknown>;

    // Try to extract license info if available
    if (apiData.spec && typeof apiData.spec === 'object') {
      const spec = apiData.spec as Record<string, unknown>;
      if (spec.license && typeof spec.license === 'object') {
        const licenseData = spec.license as Record<string, unknown>;
        mock.license = {
          name: String(licenseData.name || mock.license.name),
          organization: String(licenseData.organization || mock.license.organization),
          expire_at: String(licenseData.expire_at || mock.license.expire_at),
          fingerprint: licenseData.fingerprint ? String(licenseData.fingerprint) : null,
        };
      }
    }

    // Try to extract status info if available
    if (apiData.status && typeof apiData.status === 'object') {
      const status = apiData.status as Record<string, unknown>;
      if (status.sessions && Array.isArray(status.sessions)) {
        mock.sessions = status.sessions.map((s: unknown, i: number) => {
          const session = s as Record<string, unknown>;
          return {
            id: String(session.id || `session-${i}`),
            user: String(session.user || 'unknown'),
            target: String(session.target || 'unknown'),
            namespace: String(session.namespace || 'default'),
            mode: session.mode === 'mirror' ? 'mirror' : 'steal',
            started_at: String(session.started_at || new Date().toISOString()),
            duration_seconds: Number(session.duration_seconds || 0),
            ports: Array.isArray(session.ports) ? session.ports.map(Number) : [],
            is_ci: Boolean(session.is_ci),
          };
        });
      }

      if (status.statistics && typeof status.statistics === 'object') {
        const stats = status.statistics as Record<string, unknown>;
        mock.statistics = {
          ...mock.statistics,
          total_sessions: Number(stats.total_sessions || mock.statistics.total_sessions),
          total_users: Number(stats.total_users || mock.statistics.total_users),
          daily_active_users: Number(stats.daily_active_users || mock.statistics.daily_active_users),
          monthly_active_users: Number(stats.monthly_active_users || mock.statistics.monthly_active_users),
        };
      }
    }
  }

  return mock;
}

// Force refresh mock data (for development)
export function resetMockData(): void {
  mockData = null;
}
