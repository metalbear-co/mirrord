export interface License {
  name: string;
  organization: string;
  expire_at: string;
  fingerprint: string | null;
  tier: 'free' | 'team' | 'enterprise';
}

export interface Session {
  id: string;
  user: string;
  target: string;
  namespace: string;
  mode: 'mirror' | 'steal';
  started_at: string;
  duration_seconds: number;
  ports: number[];
  is_ci: boolean;
}

export interface Statistics {
  total_sessions: number;
  total_users: number;
  daily_active_users: number;
  monthly_active_users: number;
  sessions_by_mode: {
    mirror: number;
    steal: number;
  };
  sessions_by_target: Record<string, number>;
  sessions_by_user: Record<string, number>;
  usage_over_time: UsageDataPoint[];
}

export interface UsageDataPoint {
  date: string;
  sessions: number;
  users: number;
}

export interface OperatorStatus {
  license: License;
  sessions: Session[];
  statistics: Statistics;
  operator_version: string;
  last_updated: string;
}

export interface DateRange {
  start: Date;
  end: Date;
  preset: '7d' | '30d' | '90d' | 'custom';
}

export type SortDirection = 'asc' | 'desc';

export interface TableSort {
  column: keyof Session | null;
  direction: SortDirection;
}
