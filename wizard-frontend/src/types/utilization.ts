export interface UtilizationReport {
  generalMetrics: {
    totalLicenses: number;
    usedMachines: number;
    reportPeriod: { from: string; to: string };
    currentConcurrentSessions: number;
    maxConcurrentSessions: number;
    developmentTimeSaved: number; // hours
    totalSessionTime: number; // hours
  };
  ciMetrics: {
    currentRunningSessions: number;
    maxConcurrentCISessions: number;
    totalCISessions: number;
    avgCISessionDuration: number; // minutes
  };
  userMetrics: UserMetric[];
}

export interface UserMetric {
  machineId: string;
  kubernetesUser?: string;
  activeSince: string;
  lastSession: string;
  totalSessionTime: number; // minutes
  sessionCount: number;
  avgSessionDuration: number; // minutes
  avgDailySessions: number;
}

export interface UtilizationFilters {
  from?: string;
  to?: string;
}