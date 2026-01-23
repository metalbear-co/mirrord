import { HelpCircle } from 'lucide-react';
import {
  DataCard,
  DataCardHeader,
  DataCardTitle,
  DataCardIcon,
  DataCardContent,
  DataCardValue,
  DataCardDescription,
  DataCardTrend,
  trendVariants,
  Tooltip,
  TooltipTrigger,
  TooltipContent,
  TooltipProvider,
} from '@metalbear/ui';
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
  children?: ReactNode;
  className?: string;
  tooltip?: string;
}

export function MetricCard({
  title,
  value,
  subtitle,
  icon,
  trend,
  children,
  className,
  tooltip,
}: MetricCardProps) {
  // Map 'flat' to 'neutral' for UI kit compatibility
  const trendDirection = trend?.direction === 'flat' ? 'neutral' : trend?.direction;

  return (
    <DataCard className={classNames('card-hover', className)}>
      <DataCardHeader>
        {icon && <DataCardIcon>{icon}</DataCardIcon>}
        <div className="flex items-center gap-1.5">
          <DataCardTitle>{title}</DataCardTitle>
          {tooltip && (
            <TooltipProvider>
              <Tooltip>
                <TooltipTrigger asChild>
                  <button className="text-[var(--muted-foreground)] hover:text-[var(--foreground)] transition-colors">
                    <HelpCircle className="w-3.5 h-3.5" />
                  </button>
                </TooltipTrigger>
                <TooltipContent>
                  <p className="max-w-xs text-body-sm">{tooltip}</p>
                </TooltipContent>
              </Tooltip>
            </TooltipProvider>
          )}
        </div>
        {trend && trend.percentage > 0 && trendDirection && (
          <DataCardTrend className={trendVariants({ trend: trendDirection as 'up' | 'down' | 'neutral' })}>
            {trend.percentage}%
          </DataCardTrend>
        )}
      </DataCardHeader>

      <DataCardContent>
        <DataCardValue>{value}</DataCardValue>
        {subtitle && <DataCardDescription>{subtitle}</DataCardDescription>}
      </DataCardContent>

      {children && <div className="mt-4 pt-4 border-t border-[var(--border)]">{children}</div>}
    </DataCard>
  );
}

interface LicenseCardProps {
  organization: string;
  daysUntilExpiration: number;
  status: 'valid' | 'warning' | 'expired';
  className?: string;
}

export function LicenseCard({ organization, daysUntilExpiration, status, className }: LicenseCardProps) {
  const statusConfig = {
    valid: { label: 'Active', message: `${daysUntilExpiration} days remaining` },
    warning: { label: 'Expiring Soon', message: `${daysUntilExpiration} days remaining` },
    expired: { label: 'Expired', message: 'License has expired' },
  };

  const config = statusConfig[status];

  return (
    <MetricCard
      title="License Status"
      value={config.label}
      subtitle={organization}
      className={className}
      tooltip="Your mirrord operator license validity and expiration status"
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
      <p className="text-body-sm text-muted">{config.message}</p>
    </MetricCard>
  );
}

interface SessionsCardProps {
  activeSessions: number;
  userSessions: number;
  ciSessions: number;
  className?: string;
}

export function SessionsCard({ activeSessions, userSessions, ciSessions, className }: SessionsCardProps) {
  return (
    <MetricCard
      title="Active Sessions"
      value={activeSessions}
      className={className}
      tooltip="Currently running mirrord sessions across your cluster"
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
      <div className="flex items-center justify-between text-body-sm">
        <span className="text-muted">Users: {userSessions}</span>
        <span className="text-muted">CI: {ciSessions}</span>
      </div>
    </MetricCard>
  );
}
