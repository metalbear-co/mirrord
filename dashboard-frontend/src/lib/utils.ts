import { format, formatDistanceToNow, differenceInDays, subDays } from 'date-fns';
import type { DateRange, Session, Statistics, UsageDataPoint } from '@/types/mirrord';

export function formatDate(date: string | Date): string {
  return format(new Date(date), 'MMM d, yyyy');
}

export function formatDateTime(date: string | Date): string {
  return format(new Date(date), 'MMM d, yyyy HH:mm');
}

export function formatRelativeTime(date: string | Date): string {
  return formatDistanceToNow(new Date(date), { addSuffix: true });
}

export function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  const hours = Math.floor(seconds / 3600);
  const mins = Math.floor((seconds % 3600) / 60);
  return mins > 0 ? `${hours}h ${mins}m` : `${hours}h`;
}

export function getDaysUntilExpiration(expireAt: string): number {
  return differenceInDays(new Date(expireAt), new Date());
}

export function getLicenseStatus(expireAt: string): 'valid' | 'warning' | 'expired' {
  const days = getDaysUntilExpiration(expireAt);
  if (days < 0) return 'expired';
  if (days <= 30) return 'warning';
  return 'valid';
}

export function calculateTimeSaved(statistics: Statistics): number {
  // Estimate: each mirrord session saves ~15 minutes of deploy/wait cycle
  const minutesSavedPerSession = 15;
  return Math.round((statistics.total_sessions * minutesSavedPerSession) / 60);
}

export function getTrendIndicator(current: number, previous: number): { direction: 'up' | 'down' | 'flat'; percentage: number } {
  if (previous === 0) return { direction: 'flat', percentage: 0 };
  const change = ((current - previous) / previous) * 100;
  if (Math.abs(change) < 1) return { direction: 'flat', percentage: 0 };
  return {
    direction: change > 0 ? 'up' : 'down',
    percentage: Math.abs(Math.round(change)),
  };
}

export function filterDataByDateRange(data: UsageDataPoint[], range: DateRange): UsageDataPoint[] {
  const startTime = range.start.getTime();
  const endTime = range.end.getTime();
  return data.filter((point) => {
    const pointTime = new Date(point.date).getTime();
    return pointTime >= startTime && pointTime <= endTime;
  });
}

export function filterSessionsByDateRange(sessions: Session[], range: DateRange): Session[] {
  const startTime = range.start.getTime();
  const endTime = range.end.getTime();
  return sessions.filter((session) => {
    const sessionTime = new Date(session.started_at).getTime();
    return sessionTime >= startTime && sessionTime <= endTime;
  });
}

export function getDefaultDateRange(): DateRange {
  return {
    start: subDays(new Date(), 30),
    end: new Date(),
    preset: '30d',
  };
}

export function getDateRangeFromPreset(preset: '7d' | '30d' | '90d'): DateRange {
  const end = new Date();
  const days = preset === '7d' ? 7 : preset === '30d' ? 30 : 90;
  return {
    start: subDays(end, days),
    end,
    preset,
  };
}

export function classNames(...classes: (string | undefined | null | false)[]): string {
  return classes.filter(Boolean).join(' ');
}

export function getTopItems<T extends Record<string, number>>(
  data: T,
  limit: number = 5
): Array<{ name: string; value: number }> {
  return Object.entries(data)
    .map(([name, value]) => ({ name, value }))
    .sort((a, b) => b.value - a.value)
    .slice(0, limit);
}
