import { Search, ChevronDown, ChevronUp, ChevronRight } from 'lucide-react';
import { formatDuration, formatRelativeTime, classNames } from '@/lib/utils';
import type { Session, TableSort } from '@/types/mirrord';

interface SessionsTableProps {
  sessions: Session[];
  searchQuery: string;
  onSearchChange: (query: string) => void;
  sort: TableSort;
  onSort: (column: keyof Session) => void;
  expandedSessionId: string | null;
  onToggleExpand: (sessionId: string) => void;
}

export function SessionsTable({
  sessions,
  searchQuery,
  onSearchChange,
  sort,
  onSort,
  expandedSessionId,
  onToggleExpand,
}: SessionsTableProps) {
  const SortIcon = ({ column }: { column: keyof Session }) => {
    if (sort.column !== column) {
      return <ChevronDown className="w-4 h-4 text-[var(--muted-foreground)]" />;
    }
    return sort.direction === 'asc' ? (
      <ChevronUp className="w-4 h-4 text-primary-500" />
    ) : (
      <ChevronDown className="w-4 h-4 text-primary-500" />
    );
  };

  return (
    <div className="card">
      <div className="flex flex-col sm:flex-row items-start sm:items-center justify-between gap-4 mb-4">
        <h3 className="text-h4 font-semibold text-[var(--foreground)]">Active Sessions</h3>
        <div className="relative w-full sm:w-64">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-[var(--muted-foreground)]" />
          <input
            type="text"
            placeholder="Search sessions..."
            value={searchQuery}
            onChange={(e) => onSearchChange(e.target.value)}
            className="w-full pl-10 pr-4 py-2 bg-[var(--card)] border border-[var(--border)] rounded-lg text-[var(--foreground)] placeholder-[var(--muted-foreground)] focus:outline-none focus:border-primary focus:ring-2 focus:ring-primary/30 transition-colors"
          />
        </div>
      </div>

      <div className="overflow-x-auto">
        <table className="w-full">
          <thead>
            <tr className="border-b border-[var(--border)]">
              <th className="w-8"></th>
              <SortableHeader column="user" label="User" sort={sort} onSort={onSort} SortIcon={SortIcon} />
              <SortableHeader column="target" label="Target" sort={sort} onSort={onSort} SortIcon={SortIcon} />
              <SortableHeader column="namespace" label="Namespace" sort={sort} onSort={onSort} SortIcon={SortIcon} />
              <th className="px-4 py-3 text-left text-sm font-medium text-[var(--muted-foreground)]">Mode</th>
              <SortableHeader column="duration_seconds" label="Duration" sort={sort} onSort={onSort} SortIcon={SortIcon} />
              <SortableHeader column="started_at" label="Started" sort={sort} onSort={onSort} SortIcon={SortIcon} />
            </tr>
          </thead>
          <tbody>
            {sessions.length === 0 ? (
              <tr>
                <td colSpan={7} className="px-4 py-8 text-center text-[var(--muted-foreground)]">
                  No active sessions
                </td>
              </tr>
            ) : (
              sessions.map((session) => (
                <SessionRow
                  key={session.id}
                  session={session}
                  isExpanded={expandedSessionId === session.id}
                  onToggle={() => onToggleExpand(session.id)}
                />
              ))
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

interface SortableHeaderProps {
  column: keyof Session;
  label: string;
  sort: TableSort;
  onSort: (column: keyof Session) => void;
  SortIcon: React.ComponentType<{ column: keyof Session }>;
}

function SortableHeader({ column, label, onSort, SortIcon }: SortableHeaderProps) {
  return (
    <th
      className="px-4 py-3 text-left text-sm font-medium text-[var(--muted-foreground)] cursor-pointer hover:text-[var(--foreground)] transition-colors"
      onClick={() => onSort(column)}
    >
      <div className="flex items-center gap-1">
        {label}
        <SortIcon column={column} />
      </div>
    </th>
  );
}

interface SessionRowProps {
  session: Session;
  isExpanded: boolean;
  onToggle: () => void;
}

function SessionRow({ session, isExpanded, onToggle }: SessionRowProps) {
  return (
    <>
      <tr
        className="border-b border-[var(--border)]/50 hover:bg-[var(--muted)]/30 cursor-pointer transition-colors"
        onClick={onToggle}
      >
        <td className="px-2 py-3">
          <ChevronRight
            className={classNames(
              'w-4 h-4 text-[var(--muted-foreground)] transition-transform',
              isExpanded && 'rotate-90'
            )}
          />
        </td>
        <td className="px-4 py-3">
          <div className="flex items-center gap-2">
            <span className="text-[var(--foreground)] font-medium">{session.user}</span>
            {session.is_ci && <span className="badge badge-info">CI</span>}
          </div>
        </td>
        <td className="px-4 py-3 text-[var(--foreground)]">{session.target}</td>
        <td className="px-4 py-3 text-[var(--foreground)]">{session.namespace}</td>
        <td className="px-4 py-3">
          <span
            className={classNames(
              'badge',
              session.mode === 'steal' ? 'badge-primary' : 'badge-warning'
            )}
          >
            {session.mode}
          </span>
        </td>
        <td className="px-4 py-3 text-[var(--foreground)]">{formatDuration(session.duration_seconds)}</td>
        <td className="px-4 py-3 text-[var(--muted-foreground)] text-sm">{formatRelativeTime(session.started_at)}</td>
      </tr>
      {isExpanded && (
        <tr className="bg-[var(--muted)]/30">
          <td colSpan={7} className="px-8 py-4">
            <div className="grid grid-cols-2 sm:grid-cols-4 gap-4 text-sm">
              <div>
                <p className="text-[var(--muted-foreground)] mb-1">Session ID</p>
                <p className="text-[var(--foreground)] font-mono text-xs">{session.id}</p>
              </div>
              <div>
                <p className="text-[var(--muted-foreground)] mb-1">Ports</p>
                <div className="flex flex-wrap gap-1">
                  {session.ports.map((port) => (
                    <span key={port} className="badge badge-primary">
                      {port}
                    </span>
                  ))}
                </div>
              </div>
              <div>
                <p className="text-[var(--muted-foreground)] mb-1">Started At</p>
                <p className="text-[var(--foreground)]">{new Date(session.started_at).toLocaleString()}</p>
              </div>
              <div>
                <p className="text-[var(--muted-foreground)] mb-1">Type</p>
                <p className="text-[var(--foreground)]">{session.is_ci ? 'CI Pipeline' : 'User Session'}</p>
              </div>
            </div>
          </td>
        </tr>
      )}
    </>
  );
}
