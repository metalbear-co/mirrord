import { TrendingUp, TrendingDown, Minus } from 'lucide-react';
import { classNames } from '@/lib/utils';
import type { ReactNode } from 'react';

interface MetricCardProps {
  title: string;
  value: string | number;
  subtitle?: string;
  icon?: ReactNode;
  trend?: {
    direction: 'up' | 'down' | 'flat';
    percentage: number;
  };
  status?: 'success' | 'warning' | 'error' | 'info';
  children?: ReactNode;
}

export function MetricCard({
  title,
  value,
  subtitle,
  icon,
  trend,
  status,
  children,
}: MetricCardProps) {
  const statusColors = {
    success: 'text-green-600 dark:text-green-400',
    warning: 'text-yellow-600 dark:text-yellow-400',
    error: 'text-destructive-500 dark:text-destructive-400',
    info: 'text-blue-600 dark:text-blue-400',
  };

  const statusBgColors = {
    success: 'bg-green-500/10',
    warning: 'bg-yellow-500/10',
    error: 'bg-destructive-400/10',
    info: 'bg-blue-500/10',
  };

  const trendColors = {
    up: 'text-green-600 dark:text-green-400',
    down: 'text-destructive-500 dark:text-destructive-400',
    flat: 'text-muted',
  };

  const TrendIcon = trend?.direction === 'up' ? TrendingUp : trend?.direction === 'down' ? TrendingDown : Minus;

  return (
    <div className="card card-hover">
      <div className="flex items-start justify-between mb-4">
        <div className="flex items-center gap-3">
          {icon && (
            <div
              className={classNames(
                'w-10 h-10 rounded-lg flex items-center justify-center',
                status ? statusBgColors[status] : 'bg-primary-500/10'
              )}
            >
              <span className={status ? statusColors[status] : 'text-primary-500'}>{icon}</span>
            </div>
          )}
          <div>
            <p className="text-muted text-sm font-medium">{title}</p>
          </div>
        </div>
        {trend && trend.percentage > 0 && (
          <div className={classNames('flex items-center gap-1 text-sm', trendColors[trend.direction])}>
            <TrendIcon className="w-4 h-4" />
            <span>{trend.percentage}%</span>
          </div>
        )}
      </div>

      <div className="flex items-end justify-between">
        <div>
          <p className={classNames('text-3xl font-bold', status ? statusColors[status] : 'text-[var(--foreground)]')}>
            {value}
          </p>
          {subtitle && <p className="text-muted text-sm mt-1">{subtitle}</p>}
        </div>
      </div>

      {children && <div className="mt-4 pt-4 border-t border-[var(--border)]">{children}</div>}
    </div>
  );
}

interface LicenseCardProps {
  organization: string;
  daysUntilExpiration: number;
  status: 'valid' | 'warning' | 'expired';
}

export function LicenseCard({ organization, daysUntilExpiration, status }: LicenseCardProps) {
  const statusConfig = {
    valid: { label: 'Active', color: 'success' as const, message: `${daysUntilExpiration} days remaining` },
    warning: { label: 'Expiring Soon', color: 'warning' as const, message: `${daysUntilExpiration} days remaining` },
    expired: { label: 'Expired', color: 'error' as const, message: 'License has expired' },
  };

  const config = statusConfig[status];

  return (
    <MetricCard
      title="License Status"
      value={config.label}
      subtitle={organization}
      status={config.color}
      icon={
        <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor">
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={2}
            d="M9 12l2 2 4-4m5.618-4.016A11.955 11.955 0 0112 2.944a11.955 11.955 0 01-8.618 3.04A12.02 12.02 0 003 9c0 5.591 3.824 10.29 9 11.622 5.176-1.332 9-6.03 9-11.622 0-1.042-.133-2.052-.382-3.016z"
          />
        </svg>
      }
    >
      <p className="text-sm text-muted">{config.message}</p>
    </MetricCard>
  );
}

interface SessionsCardProps {
  activeSessions: number;
  userSessions: number;
  ciSessions: number;
}

export function SessionsCard({ activeSessions, userSessions, ciSessions }: SessionsCardProps) {
  return (
    <MetricCard
      title="Active Sessions"
      value={activeSessions}
      icon={
        <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor">
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={2}
            d="M13 10V3L4 14h7v7l9-11h-7z"
          />
        </svg>
      }
    >
      <div className="flex items-center justify-between text-sm">
        <span className="text-muted">Users: {userSessions}</span>
        <span className="text-muted">CI: {ciSessions}</span>
      </div>
    </MetricCard>
  );
}
