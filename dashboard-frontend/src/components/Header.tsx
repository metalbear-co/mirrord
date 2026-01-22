import { RefreshCw } from 'lucide-react';
import { formatDateTime } from '@/lib/utils';

interface HeaderProps {
  lastUpdated: Date | null;
  onRefresh: () => void;
  isLoading: boolean;
}

export function Header({ lastUpdated, onRefresh, isLoading }: HeaderProps) {
  return (
    <header className="flex flex-col sm:flex-row items-start sm:items-center justify-between gap-4 mb-8">
      <div>
        <h1 className="text-2xl font-bold text-[var(--foreground)]">Utilization Dashboard</h1>
        <p className="text-muted text-sm">Monitor your team's mirrord usage</p>
      </div>

      <div className="flex items-center gap-3">
        {lastUpdated && (
          <span className="text-muted text-sm hidden md:inline">
            Last updated: {formatDateTime(lastUpdated)}
          </span>
        )}

        <button
          onClick={onRefresh}
          disabled={isLoading}
          className="btn-primary flex items-center gap-2 disabled:opacity-70 disabled:cursor-not-allowed min-w-[120px] justify-center"
        >
          <RefreshCw className={`w-4 h-4 ${isLoading ? 'animate-spin' : ''}`} />
          {isLoading ? 'Refreshing...' : 'Refresh'}
        </button>
      </div>
    </header>
  );
}
