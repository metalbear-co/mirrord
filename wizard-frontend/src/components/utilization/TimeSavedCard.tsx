import { useState } from "react";
import { Clock, Zap, Calculator } from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { TimeSavedConfigDialog } from "./TimeSavedConfigDialog";
interface TimeSavedConfig {
  ciRunTimeMinutes: number;
  calculationMethod: 'ci-runtime' | 'manual-estimation';
  manualEstimationMinutes: number;
}
interface TimeSavedCardProps {
  title: string;
  value: number; // in hours
  subtitle?: string;
  showAdjustButton?: boolean;
  generalData?: {
    currentConcurrentSessions: number;
  };
  ciData?: {
    currentRunningSessions: number;
  };
}
export const TimeSavedCard = ({
  title,
  value,
  subtitle,
  showAdjustButton = false,
  generalData,
  ciData
}: TimeSavedCardProps) => {
  const [timeSavedConfig, setTimeSavedConfig] = useState<TimeSavedConfig>({
    ciRunTimeMinutes: 15,
    calculationMethod: 'ci-runtime',
    manualEstimationMinutes: 10
  });
  const [showConfigDialog, setShowConfigDialog] = useState(false);
  const formatTimeSaved = (hours: number) => {
    const years = Math.floor(hours / (365 * 24));
    const months = Math.floor(hours % (365 * 24) / (30 * 24));
    const weeks = Math.floor(hours % (30 * 24) / (7 * 24));
    const days = Math.floor(hours % (7 * 24) / 24);
    const remainingHours = Math.floor(hours % 24);
    const parts = [];
    if (years > 0) parts.push(`${years}y`);
    if (months > 0) parts.push(`${months}m`);
    if (weeks > 0) parts.push(`${weeks}w`);
    if (days > 0) parts.push(`${days}d`);
    if (remainingHours > 0) parts.push(`${remainingHours}h`);

    // Return only the 2 highest time frames
    return parts.slice(0, 2).join(' ') || '0h';
  };
  const calculateTimeSaved = () => {
    if (!showAdjustButton || !generalData || !ciData) return value;
    if (timeSavedConfig.calculationMethod === 'ci-runtime') {
      return timeSavedConfig.ciRunTimeMinutes / 60 * ciData.currentRunningSessions;
    } else {
      return timeSavedConfig.manualEstimationMinutes / 60 * generalData.currentConcurrentSessions;
    }
  };
  const displayValue = showAdjustButton ? calculateTimeSaved() : value;
  return <>
      <Card className="bg-gradient-to-r from-purple-50/40 to-indigo-50/40 dark:from-purple-950/15 dark:to-indigo-950/15 border-purple-200/50 dark:border-purple-800/30">
        <CardContent className="p-2">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              <div className="flex items-center gap-2">
                <Calculator className="h-4 w-4 text-purple-600 dark:text-purple-400" />
                <span className="text-sm font-medium text-purple-900 dark:text-purple-100">
                  {title}
                </span>
              </div>
              <div className="bg-white/70 dark:bg-gray-900/50 rounded-md px-2 py-1 border border-purple-100/60 dark:border-purple-800/30">
                <span className="text-base font-bold text-purple-900 dark:text-purple-100">
                  {formatTimeSaved(displayValue)}
                </span>
              </div>
            </div>
            <div className="flex items-center gap-3">
              
              {showAdjustButton && <button className="flex items-center gap-1 px-2 py-1 text-xs rounded-md border border-purple-300/40 dark:border-purple-700/40 bg-purple-100/40 dark:bg-purple-900/20 hover:bg-purple-200/50 dark:hover:bg-purple-800/30 text-purple-800 dark:text-purple-200 transition-colors" onClick={() => setShowConfigDialog(true)}>
                  <Zap className="h-3 w-3" />
                  adjust
                </button>}
            </div>
          </div>
        </CardContent>
      </Card>

      {showAdjustButton && generalData && ciData && <TimeSavedConfigDialog open={showConfigDialog} onOpenChange={setShowConfigDialog} config={timeSavedConfig} onConfigChange={setTimeSavedConfig} totalSessions={generalData.currentConcurrentSessions} ciSessions={ciData.currentRunningSessions} />}
    </>;
};