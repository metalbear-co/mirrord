import { RefreshCw, Sun, Moon } from 'lucide-react';
import { MirrordIcon } from '@metalbear/ui';
import { formatDateTime } from '@/lib/utils';

interface HeaderProps {
  lastUpdated: Date | null;
  onRefresh: () => void;
  isLoading: boolean;
  isDarkMode: boolean;
  onToggleDarkMode: () => void;
}

export function Header({ lastUpdated, onRefresh, isLoading, isDarkMode, onToggleDarkMode }: HeaderProps) {
  return (
    <header className="flex flex-col sm:flex-row items-start sm:items-center justify-between gap-4 mb-8">
      <div className="flex items-center gap-4">
        <div className="w-12 h-12 flex items-center justify-center">
          {/* TODO: Switch to MirrordIconWhite for dark mode */}
          <img
            src={MirrordIcon}
            alt="mirrord"
            className="w-12 h-12"
          />
        </div>
        <div>
          <h1 className="text-2xl font-bold text-[var(--foreground)]">Utilization Dashboard</h1>
          <p className="text-muted text-sm">Monitor your team's mirrord usage</p>
        </div>
      </div>

      <div className="flex items-center gap-3">
        {lastUpdated && (
          <span className="text-muted text-sm hidden md:inline">
            Last updated: {formatDateTime(lastUpdated)}
          </span>
        )}

        <button
          onClick={onToggleDarkMode}
          className="btn-secondary p-2"
          aria-label={isDarkMode ? 'Switch to light mode' : 'Switch to dark mode'}
        >
          {isDarkMode ? <Sun className="w-5 h-5" /> : <Moon className="w-5 h-5" />}
        </button>

        <button
          onClick={onRefresh}
          disabled={isLoading}
          className="btn-primary flex items-center gap-2 disabled:opacity-50"
        >
          <RefreshCw className={`w-4 h-4 ${isLoading ? 'animate-spin' : ''}`} />
          Refresh
        </button>
      </div>
    </header>
  );
}
