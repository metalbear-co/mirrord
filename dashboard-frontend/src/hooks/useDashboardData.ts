import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useState, useCallback, useMemo } from 'react';
import { fetchOperatorStatus, resetMockData } from '@/api/mirrordApi';
import type { DateRange, OperatorStatus, Session, TableSort } from '@/types/mirrord';
import {
  getDefaultDateRange,
  getDateRangeFromPreset,
  filterDataByDateRange,
  filterSessionsByDateRange,
} from '@/lib/utils';

export function useDashboardData() {
  const queryClient = useQueryClient();

  const {
    data,
    isLoading,
    isError,
    error,
    refetch,
    dataUpdatedAt,
  } = useQuery<OperatorStatus>({
    queryKey: ['operatorStatus'],
    queryFn: fetchOperatorStatus,
    refetchOnMount: true,
  });

  const refresh = useCallback(async () => {
    resetMockData();
    await refetch();
  }, [refetch]);

  const invalidate = useCallback(() => {
    queryClient.invalidateQueries({ queryKey: ['operatorStatus'] });
  }, [queryClient]);

  return {
    data,
    isLoading,
    isError,
    error,
    refresh,
    invalidate,
    lastUpdated: dataUpdatedAt ? new Date(dataUpdatedAt) : null,
  };
}

export function useDateRange() {
  const [dateRange, setDateRange] = useState<DateRange>(getDefaultDateRange);

  const setPreset = useCallback((preset: '7d' | '30d' | '90d') => {
    setDateRange(getDateRangeFromPreset(preset));
  }, []);

  const setCustomRange = useCallback((start: Date, end: Date) => {
    setDateRange({ start, end, preset: 'custom' });
  }, []);

  return {
    dateRange,
    setPreset,
    setCustomRange,
  };
}

export function useFilteredData(data: OperatorStatus | undefined, dateRange: DateRange) {
  return useMemo(() => {
    if (!data) return null;

    const filteredUsageData = filterDataByDateRange(data.statistics.usage_over_time, dateRange);
    const filteredSessions = filterSessionsByDateRange(data.sessions, dateRange);

    // Recalculate statistics based on filtered data
    const filteredStats = {
      ...data.statistics,
      usage_over_time: filteredUsageData,
    };

    return {
      ...data,
      statistics: filteredStats,
      sessions: filteredSessions,
    };
  }, [data, dateRange]);
}

export function useSessionsTable(sessions: Session[]) {
  const [searchQuery, setSearchQuery] = useState('');
  const [sort, setSort] = useState<TableSort>({ column: 'started_at', direction: 'desc' });
  const [expandedSessionId, setExpandedSessionId] = useState<string | null>(null);

  const filteredSessions = useMemo(() => {
    let result = [...sessions];

    // Apply search filter
    if (searchQuery) {
      const query = searchQuery.toLowerCase();
      result = result.filter(
        (session) =>
          session.user.toLowerCase().includes(query) ||
          session.target.toLowerCase().includes(query) ||
          session.namespace.toLowerCase().includes(query)
      );
    }

    // Apply sorting
    if (sort.column) {
      result.sort((a, b) => {
        const aVal = a[sort.column!];
        const bVal = b[sort.column!];

        let comparison = 0;
        if (typeof aVal === 'string' && typeof bVal === 'string') {
          comparison = aVal.localeCompare(bVal);
        } else if (typeof aVal === 'number' && typeof bVal === 'number') {
          comparison = aVal - bVal;
        }

        return sort.direction === 'asc' ? comparison : -comparison;
      });
    }

    return result;
  }, [sessions, searchQuery, sort]);

  const toggleSort = useCallback((column: keyof Session) => {
    setSort((prev) => ({
      column,
      direction: prev.column === column && prev.direction === 'asc' ? 'desc' : 'asc',
    }));
  }, []);

  const toggleExpanded = useCallback((sessionId: string) => {
    setExpandedSessionId((prev) => (prev === sessionId ? null : sessionId));
  }, []);

  return {
    filteredSessions,
    searchQuery,
    setSearchQuery,
    sort,
    toggleSort,
    expandedSessionId,
    toggleExpanded,
  };
}
