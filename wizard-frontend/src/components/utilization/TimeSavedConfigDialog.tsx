import { useState } from "react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

import { Separator } from "@/components/ui/separator";

interface TimeSavedConfig {
  ciRunTimeMinutes: number;
  calculationMethod: 'ci-runtime' | 'manual-estimation';
  manualEstimationMinutes: number;
}

interface TimeSavedConfigDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  config: TimeSavedConfig;
  onConfigChange: (config: TimeSavedConfig) => void;
  totalSessions: number;
  ciSessions: number;
}

export const TimeSavedConfigDialog = ({
  open,
  onOpenChange,
  config,
  onConfigChange,
  totalSessions,
  ciSessions,
}: TimeSavedConfigDialogProps) => {
  const [localConfig, setLocalConfig] = useState<TimeSavedConfig>(config);

  const handleSave = () => {
    onConfigChange(localConfig);
    onOpenChange(false);
  };

  const handleCancel = () => {
    setLocalConfig(config);
    onOpenChange(false);
  };

  const calculatePreview = () => {
    if (localConfig.calculationMethod === 'ci-runtime') {
      const ciHours = (localConfig.ciRunTimeMinutes / 60) * ciSessions;
      return `${localConfig.ciRunTimeMinutes} min × ${ciSessions} CI sessions = ${ciHours.toFixed(1)} hours`;
    } else {
      const totalHours = (localConfig.manualEstimationMinutes / 60) * totalSessions;
      return `${localConfig.manualEstimationMinutes} min × ${totalSessions} total sessions = ${totalHours.toFixed(1)} hours`;
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[425px]">
        <DialogHeader>
          <DialogTitle>Configure Time Saved Calculation</DialogTitle>
          <DialogDescription>
            Choose how you want to calculate the development time saved by mirrord.
          </DialogDescription>
        </DialogHeader>
        
        <div className="space-y-6 py-4">
          <div className="space-y-4">
            <div>
              <div className="font-medium mb-2">Calculation Method</div>
              <div className="space-y-3">
                <label className="flex items-center space-x-2">
                  <input
                    type="radio"
                    name="calculationMethod"
                    value="ci-runtime"
                    checked={localConfig.calculationMethod === 'ci-runtime'}
                    onChange={(e) =>
                      setLocalConfig({ ...localConfig, calculationMethod: e.target.value as 'ci-runtime' | 'manual-estimation' })
                    }
                  />
                  <span className="text-sm">CI runtime-based calculation</span>
                </label>
                <label className="flex items-center space-x-2">
                  <input
                    type="radio"
                    name="calculationMethod"
                    value="manual-estimation"
                    checked={localConfig.calculationMethod === 'manual-estimation'}
                    onChange={(e) =>
                      setLocalConfig({ ...localConfig, calculationMethod: e.target.value as 'ci-runtime' | 'manual-estimation' })
                    }
                  />
                  <span className="text-sm">Manual time estimation per session</span>
                </label>
              </div>
            </div>
            
            {localConfig.calculationMethod === 'ci-runtime' ? (
              <div className="space-y-2">
                <Label htmlFor="ci-runtime">Average CI run time (minutes)</Label>
                <Input
                  id="ci-runtime"
                  type="number"
                  value={localConfig.ciRunTimeMinutes}
                  onChange={(e) =>
                    setLocalConfig({
                      ...localConfig,
                      ciRunTimeMinutes: parseInt(e.target.value) || 0,
                    })
                  }
                  placeholder="e.g., 15"
                />
                <div className="text-xs text-muted-foreground">
                  Time saved = CI runtime × CI sessions
                </div>
              </div>
            ) : (
              <div className="space-y-2">
                <Label htmlFor="manual-estimation">Time saved per session (minutes)</Label>
                <Input
                  id="manual-estimation"
                  type="number"
                  value={localConfig.manualEstimationMinutes}
                  onChange={(e) =>
                    setLocalConfig({
                      ...localConfig,
                      manualEstimationMinutes: parseInt(e.target.value) || 0,
                    })
                  }
                  placeholder="e.g., 10"
                />
                <div className="text-xs text-muted-foreground">
                  Time saved = estimation × total sessions
                </div>
              </div>
            )}
          </div>
          
          <Separator />
          
          <div className="space-y-2">
            <Label className="text-sm font-medium">Preview</Label>
            <div className="text-sm text-muted-foreground bg-muted p-3 rounded-md">
              {calculatePreview()}
            </div>
          </div>
        </div>
        
        <DialogFooter>
          <Button variant="outline" onClick={handleCancel}>
            Cancel
          </Button>
          <Button onClick={handleSave}>Save Changes</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};