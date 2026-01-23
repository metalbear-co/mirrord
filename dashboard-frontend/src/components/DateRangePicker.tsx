import { useState } from 'react';
import { Calendar } from 'lucide-react';
import { format } from 'date-fns';
import { classNames } from '@/lib/utils';
import type { DateRange } from '@/types/mirrord';

interface DateRangePickerProps {
  dateRange: DateRange;
  onPresetChange: (preset: '7d' | '30d' | '90d') => void;
  onCustomRangeChange: (start: Date, end: Date) => void;
}

export function DateRangePicker({
  dateRange,
  onPresetChange,
  onCustomRangeChange,
}: DateRangePickerProps) {
  const [showCustom, setShowCustom] = useState(false);
  const [customStart, setCustomStart] = useState(format(dateRange.start, 'yyyy-MM-dd'));
  const [customEnd, setCustomEnd] = useState(format(dateRange.end, 'yyyy-MM-dd'));

  const presets: Array<{ value: '7d' | '30d' | '90d'; label: string }> = [
    { value: '7d', label: '7 Days' },
    { value: '30d', label: '30 Days' },
    { value: '90d', label: '90 Days' },
  ];

  const handlePresetClick = (preset: '7d' | '30d' | '90d') => {
    setShowCustom(false);
    onPresetChange(preset);
  };

  const handleApplyCustom = () => {
    const start = new Date(customStart);
    const end = new Date(customEnd);
    if (start <= end) {
      onCustomRangeChange(start, end);
      setShowCustom(false);
    }
  };

  return (
    <div className="flex flex-col sm:flex-row items-start sm:items-center gap-4">
      <div className="flex items-center gap-2">
        <Calendar className="w-4 h-4 text-[var(--muted-foreground)]" />
        <span className="text-[var(--muted-foreground)] text-body-sm">Date Range:</span>
      </div>

      <div className="flex flex-wrap items-center gap-2">
        {presets.map((preset) => (
          <button
            key={preset.value}
            onClick={() => handlePresetClick(preset.value)}
            className={classNames(
              'px-3 py-1.5 rounded-lg text-body-sm font-medium transition-all active:scale-[0.98]',
              dateRange.preset === preset.value && !showCustom
                ? 'bg-primary text-white shadow-[-7px_6.5px_0px_rgba(0,0,0,1)] active:shadow-none'
                : 'bg-[var(--card)] text-[var(--foreground)] hover:shadow-[-7px_6.5px_0px_rgba(0,0,0,1)] active:shadow-none border border-[var(--border)]'
            )}
          >
            {preset.label}
          </button>
        ))}

        <button
          onClick={() => setShowCustom(!showCustom)}
          className={classNames(
            'px-3 py-1.5 rounded-lg text-body-sm font-medium transition-all active:scale-[0.98]',
            dateRange.preset === 'custom' || showCustom
              ? 'bg-primary text-white shadow-[-7px_6.5px_0px_rgba(0,0,0,1)] active:shadow-none'
              : 'bg-[var(--card)] text-[var(--foreground)] hover:shadow-[-7px_6.5px_0px_rgba(0,0,0,1)] active:shadow-none border border-[var(--border)]'
          )}
        >
          Custom
        </button>
      </div>

      {showCustom && (
        <div className="flex flex-wrap items-center gap-2 mt-2 sm:mt-0">
          <input
            type="date"
            value={customStart}
            onChange={(e) => setCustomStart(e.target.value)}
            className="px-3 py-1.5 bg-[var(--card)] border border-[var(--border)] rounded-lg text-[var(--foreground)] text-body-sm focus:outline-none focus:border-primary focus:ring-2 focus:ring-primary/30"
          />
          <span className="text-[var(--muted-foreground)]">to</span>
          <input
            type="date"
            value={customEnd}
            onChange={(e) => setCustomEnd(e.target.value)}
            className="px-3 py-1.5 bg-[var(--card)] border border-[var(--border)] rounded-lg text-[var(--foreground)] text-body-sm focus:outline-none focus:border-primary focus:ring-2 focus:ring-primary/30"
          />
          <button onClick={handleApplyCustom} className="btn-primary text-body-sm py-1.5">
            Apply
          </button>
        </div>
      )}

      {dateRange.preset === 'custom' && !showCustom && (
        <span className="text-[var(--foreground)] text-body-sm">
          {format(dateRange.start, 'MMM d, yyyy')} - {format(dateRange.end, 'MMM d, yyyy')}
        </span>
      )}
    </div>
  );
}
