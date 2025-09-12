import { UtilizationReport, UtilizationFilters } from "@/types/utilization";

// Mock data generator
const generateMockData = (filters?: UtilizationFilters): UtilizationReport => {
  const now = new Date();
  const defaultFrom = new Date(now.getTime() - 7 * 24 * 60 * 60 * 1000); // 7 days ago
  const from = filters?.from ? new Date(filters.from) : defaultFrom;
  const to = filters?.to ? new Date(filters.to) : now;

  const mockUsers = [
    { machineId: "DEV-001", kubernetesUser: "alice.dev", activeSince: "2024-03-15", baseActivity: 150 },
    { machineId: "DEV-002", kubernetesUser: "bob.backend", activeSince: "2024-02-20", baseActivity: 120 },
    { machineId: "DEV-003", kubernetesUser: "charlie.frontend", activeSince: "2024-04-01", baseActivity: 95 },
    { machineId: "DEV-004", kubernetesUser: "diana.qa", activeSince: "2024-01-10", baseActivity: 80 },
    { machineId: "DEV-005", kubernetesUser: "eve.devops", activeSince: "2024-03-05", baseActivity: 110 },
    { machineId: "DEV-006", kubernetesUser: "frank.mobile", activeSince: "2024-04-15", baseActivity: 70 },
    { machineId: "DEV-007", kubernetesUser: "grace.data", activeSince: "2024-02-01", baseActivity: 85 },
    { machineId: "DEV-008", kubernetesUser: "henry.security", activeSince: "2024-03-20", baseActivity: 60 },
    { machineId: "DEV-009", kubernetesUser: "iris.design", activeSince: "2024-04-08", baseActivity: 45 },
    { machineId: "DEV-010", kubernetesUser: "jack.intern", activeSince: "2024-05-01", baseActivity: 30 },
  ];

  const userMetrics = mockUsers.map((user) => {
    const sessionCount = Math.floor(user.baseActivity * 0.6) + Math.floor(Math.random() * 20);
    const totalSessionTime = Math.floor(user.baseActivity * (0.8 + Math.random() * 0.4));
    const avgSessionDuration = Math.floor(totalSessionTime / sessionCount);
    const daysSinceActive = Math.floor((now.getTime() - new Date(user.activeSince).getTime()) / (24 * 60 * 60 * 1000));
    const avgDailySessions = parseFloat((sessionCount / Math.max(daysSinceActive, 1)).toFixed(1));

    // Random last session within the last 30 days
    const lastSessionDate = new Date(now.getTime() - Math.random() * 30 * 24 * 60 * 60 * 1000);

    return {
      machineId: user.machineId,
      kubernetesUser: user.kubernetesUser,
      activeSince: user.activeSince,
      lastSession: lastSessionDate.toISOString().split('T')[0],
      totalSessionTime,
      sessionCount,
      avgSessionDuration,
      avgDailySessions,
    };
  });

  // Sort by total session time (most active first)
  userMetrics.sort((a, b) => b.totalSessionTime - a.totalSessionTime);

  const currentConcurrentSessions = Math.floor(Math.random() * 15) + 8; // 8-23 concurrent sessions
  const maxConcurrentSessions = Math.floor(Math.random() * 25) + 35; // 35-60 max concurrent
  
  // Generate CI metrics
  const currentRunningSessions = Math.floor(Math.random() * 8) + 2; // 2-10 running CI sessions
  const maxConcurrentCISessions = Math.floor(Math.random() * 15) + 20; // 20-35 max CI concurrent
  const totalCISessions = Math.floor(Math.random() * 500) + 200; // 200-700 total CI sessions
  const avgCISessionDuration = Math.floor(Math.random() * 45) + 15; // 15-60 minutes

  return {
    generalMetrics: {
      totalLicenses: 50,
      usedMachines: userMetrics.length,
      reportPeriod: {
        from: from.toISOString().split('T')[0],
        to: to.toISOString().split('T')[0],
      },
      currentConcurrentSessions,
      maxConcurrentSessions,
      developmentTimeSaved: Math.floor(Math.random() * 500) + 200, // 200-700 hours saved
      totalSessionTime: Math.floor(Math.random() * 2000) + 1500, // 1500-3500 total hours
    },
    ciMetrics: {
      currentRunningSessions,
      maxConcurrentCISessions,
      totalCISessions,
      avgCISessionDuration,
    },
    userMetrics,
  };
};

export const fetchUtilizationReport = async (filters?: UtilizationFilters): Promise<UtilizationReport> => {
  // Simulate API delay
  await new Promise(resolve => setTimeout(resolve, 800));
  return generateMockData(filters);
};

export const exportUtilizationReport = async (filters?: UtilizationFilters): Promise<void> => {
  // Simulate export functionality
  await new Promise(resolve => setTimeout(resolve, 1000));
  console.log('Exporting utilization report with filters:', filters);
  // In a real implementation, this would trigger file download
};