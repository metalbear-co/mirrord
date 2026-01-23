import { MirrordIcon } from '@metalbear/ui';
import { Sun, Moon, ExternalLink, RefreshCw } from 'lucide-react';
import { formatDateTime } from '@/lib/utils';

interface AppBarProps {
  isDarkMode: boolean;
  onToggleDarkMode: () => void;
  lastUpdated: Date | null;
  onRefresh: () => void;
  isLoading: boolean;
}

export function AppBar({ isDarkMode, onToggleDarkMode, lastUpdated, onRefresh, isLoading }: AppBarProps) {
  return (
    <div className={isDarkMode ? 'bg-[#232141]' : 'bg-white border-b border-[#E5E5E5]'}>
      <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
        <div className={`flex items-center justify-between h-14 ${isDarkMode ? 'text-white' : 'text-[#232141]'}`}>
          {/* Logo and brand */}
          <div className="flex items-center gap-3">
            <img
              src={MirrordIcon}
              alt="mirrord"
              className={`w-8 h-8 ${isDarkMode ? 'invert' : ''}`}
            />
            <div className="hidden sm:flex items-center gap-2">
              <span className="font-semibold text-h4">mirrord</span>
              <span className={isDarkMode ? 'text-white/30' : 'text-[#232141]/30'}>|</span>
              <span className={`text-body-sm font-medium ${isDarkMode ? 'text-white/80' : 'text-[#232141]/70'}`}>Utilization</span>
            </div>
            <span className="font-semibold text-h4 sm:hidden">mirrord</span>
          </div>

          {/* Right side actions */}
          <div className="flex items-center gap-3">
            {lastUpdated && (
              <span className={`text-body-sm hidden lg:inline ${isDarkMode ? 'text-white/50' : 'text-[#232141]/50'}`}>
                {formatDateTime(lastUpdated)}
              </span>
            )}

            <button
              onClick={onRefresh}
              disabled={isLoading}
              className="flex items-center gap-2 px-3 py-1.5 bg-[#756DF3] hover:bg-[#756DF3]/90 active:scale-[0.98] text-white rounded-lg text-body-sm font-medium transition-all disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <RefreshCw className={`w-4 h-4 ${isLoading ? 'animate-spin' : ''}`} />
              <span className="hidden sm:inline">{isLoading ? 'Syncing...' : 'Sync'}</span>
            </button>

            <div className="hidden md:flex items-center gap-3">
              <a
                href="https://mirrord.dev/docs"
                target="_blank"
                rel="noopener noreferrer"
                className={`text-body-sm flex items-center gap-1 transition-colors ${isDarkMode ? 'text-white/60 hover:text-white' : 'text-[#232141]/60 hover:text-[#756DF3]'}`}
              >
                Docs
                <ExternalLink className="w-3 h-3" />
              </a>
              <a
                href="https://app.metalbear.co"
                target="_blank"
                rel="noopener noreferrer"
                className={`text-body-sm flex items-center gap-1 transition-colors ${isDarkMode ? 'text-white/60 hover:text-white' : 'text-[#232141]/60 hover:text-[#756DF3]'}`}
              >
                Console
                <ExternalLink className="w-3 h-3" />
              </a>
            </div>

            <button
              onClick={onToggleDarkMode}
              className={`p-2 rounded-lg transition-all active:scale-[0.95] ${isDarkMode ? 'hover:bg-white/10' : 'hover:bg-[#232141]/5'}`}
              aria-label={isDarkMode ? 'Switch to light mode' : 'Switch to dark mode'}
            >
              {isDarkMode ? <Sun className="w-5 h-5" /> : <Moon className="w-5 h-5" />}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
