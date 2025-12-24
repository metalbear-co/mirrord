import { useState } from "react";
import { BadgeCheck, ChartNoAxesCombined, Timer, Users, Zap } from "lucide-react";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "../ui/dialog";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { Label } from "../ui/label";
import { Separator } from "../ui/separator";
import type { AdminAllTimeMetricsData } from "./adminMetricsData";

interface AdminAllTimeMetricsProps {
  data: AdminAllTimeMetricsData;
}

const AdminAllTimeMetrics = ({ data }: AdminAllTimeMetricsProps) => {
  const [isAdjustOpen, setIsAdjustOpen] = useState(false);
  const hideCi = data.tier.toLowerCase() === "teams";
  // TODO: Backend wiring: initialize from persisted calculation settings.
  const [avgUserSessionMinutes, setAvgUserSessionMinutes] = useState(
    String(data.avgUserSessionMinutes),
  );
  const [avgCiWaitMinutes, setAvgCiWaitMinutes] = useState(
    String(data.avgCiWaitMinutes),
  );

  const parseMinutes = (value: string) => {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : 0;
  };

  const formatHours = (value: number) => {
    const rounded = Math.round(value * 10) / 10;
    return rounded % 1 === 0 ? rounded.toFixed(0) : rounded.toFixed(1);
  };

  const userMinutes = parseMinutes(avgUserSessionMinutes);
  const ciMinutes = parseMinutes(avgCiWaitMinutes);
  const userHours = (userMinutes * data.totalUserSessions) / 60;
  const ciHours = (ciMinutes * data.totalCiSessions) / 60;

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <h2 className="text-xl sm:text-2xl font-semibold text-foreground">
          All Time Metrics
        </h2>
      </div>

      <div className="grid gap-4 sm:gap-6 xl:grid-cols-[1.2fr_2fr]">
        <div className="rounded-3xl border border-primary/30 bg-gradient-primary/10 p-6 sm:p-8 shadow-glow">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2 text-primary">
              <Zap className="h-5 w-5" />
              <span className="text-sm font-semibold uppercase tracking-[0.2em]">
                Dev Time Saved
              </span>
            </div>
            <button
              className="inline-flex items-center gap-2 rounded-xl border border-primary/40 px-3 py-1 text-sm font-semibold text-primary"
              onClick={() => setIsAdjustOpen(true)}
            >
              <BadgeCheck className="h-4 w-4" />
              adjust
            </button>
          </div>
          <div className="mt-6 grid gap-3 sm:grid-cols-2">
            <div className="rounded-2xl bg-background/80 px-4 py-4 text-center">
              <p className="text-3xl sm:text-4xl font-semibold text-foreground">
                {formatHours(userHours)}h
              </p>
              <p className="mt-1 text-xs uppercase tracking-[0.2em] text-muted-foreground">
                Developer Sessions
              </p>
            </div>
            {!hideCi && (
              <div className="rounded-2xl bg-background/80 px-4 py-4 text-center">
                <p className="text-3xl sm:text-4xl font-semibold text-foreground">
                  {formatHours(ciHours)}h
                </p>
                <p className="mt-1 text-xs uppercase tracking-[0.2em] text-muted-foreground">
                  CI Waiting
                </p>
              </div>
            )}
          </div>
          <p className="mt-4 text-sm text-muted-foreground">
            Combined time saved since adopting mirrord.
          </p>
        </div>

        <div className="grid gap-4 sm:gap-6 md:grid-cols-2">
        <div className="rounded-2xl border border-border/60 bg-gradient-card p-5 sm:p-6 shadow-sm">
          <div className="flex items-center justify-between text-muted-foreground">
            <p className="text-sm font-semibold">Licenses</p>
            <Users className="h-5 w-5" />
          </div>
          <div className="mt-5 rounded-xl bg-background/80 px-4 py-3 text-center">
            <p className="text-3xl font-semibold text-foreground">
              {data.licensesUsed}{" "}
              <span className="text-muted-foreground">
                / {data.licensesTotal}
              </span>
            </p>
          </div>
          <p className="mt-3 text-center text-sm text-muted-foreground">
            used / acquired licenses
          </p>
        </div>

        <div className="rounded-2xl border border-border/60 bg-gradient-card p-5 sm:p-6 shadow-sm">
          <div className="flex items-center justify-between text-muted-foreground">
            <p className="text-sm font-semibold">Active Sessions</p>
            <ChartNoAxesCombined className="h-5 w-5 text-emerald-500" />
          </div>
          <div className="mt-5 grid gap-3">
            <div className="rounded-xl bg-background/80 px-4 py-3 text-center">
              <p className="text-3xl font-semibold text-foreground">
                {data.activeUserSessions}{" "}
                <span className="text-sm text-muted-foreground">user</span>
              </p>
            </div>
            {!hideCi && (
              <>
                <div className="rounded-xl bg-background/80 px-4 py-3 text-center">
                  <p className="text-3xl font-semibold text-foreground">
                    {data.activeCiSessions}{" "}
                    <span className="text-sm text-muted-foreground">CI</span>
                  </p>
                </div>
                <a
                  className="text-xs font-semibold text-primary underline-offset-4 hover:underline text-center"
                  href="https://metalbear.com/mirrord/docs/using-mirrord/mirrord-for-ci"
                  target="_blank"
                  rel="noreferrer"
                >
                  Learn more about CI
                </a>
              </>
            )}
          </div>
        </div>

        <div className="rounded-2xl border border-border/60 bg-gradient-card p-5 sm:p-6 shadow-sm">
          <div className="flex items-center justify-between text-muted-foreground">
            <p className="text-sm font-semibold">Max Concurrency</p>
            <ChartNoAxesCombined className="h-5 w-5" />
          </div>
          <div className="mt-5 grid gap-3">
            <div className="rounded-xl bg-background/80 px-4 py-3 text-center">
              <p className="text-3xl font-semibold text-foreground">
                {data.maxUserConcurrency}{" "}
                <span className="text-sm text-muted-foreground">user</span>
              </p>
            </div>
            {!hideCi && (
              <div className="rounded-xl bg-background/80 px-4 py-3 text-center">
                <p className="text-3xl font-semibold text-foreground">
                  {data.maxCiConcurrency}{" "}
                  <span className="text-sm text-muted-foreground">CI</span>
                </p>
              </div>
            )}
          </div>
        </div>

        <div className="rounded-2xl border border-border/60 bg-gradient-card p-5 sm:p-6 shadow-sm">
          <div className="flex items-center justify-between text-muted-foreground">
            <p className="text-sm font-semibold">Total Session Time</p>
            <Timer className="h-5 w-5" />
          </div>
          <div className="mt-5 rounded-xl bg-background/80 px-4 py-6 text-center">
            <p className="text-3xl font-semibold text-foreground">
              {data.totalSessionTime}
            </p>
          </div>
          <p className="mt-3 text-center text-sm text-muted-foreground">
            total development time (months & weeks)
          </p>
        </div>
      </div>
      </div>

      <Dialog open={isAdjustOpen} onOpenChange={setIsAdjustOpen}>
        <DialogContent className="max-w-3xl p-0 overflow-hidden">
          <DialogDescription className="sr-only">
            Configure time saved calculation
          </DialogDescription>
          <div className="p-6 sm:p-8">
            <div className="flex items-start justify-between gap-4">
              <div>
                <DialogTitle className="text-2xl font-semibold text-foreground">
                  Configure Time Saved Calculation
                </DialogTitle>
                <p className="mt-2 text-sm text-muted-foreground">
                  Choose how you want to calculate development time saved by mirrord.
                </p>
              </div>
            </div>

            <div className="mt-6 space-y-6">
              <div className="space-y-2">
                <div className="flex items-center justify-between gap-2">
                  <Label htmlFor="avg-user-session">
                    Average time saved per user session (minutes)
                  </Label>
                  <span className="rounded-full bg-muted px-2 py-1 text-xs font-semibold text-muted-foreground">
                    {data.totalUserSessions} sessions
                  </span>
                </div>
                <Input
                  id="avg-user-session"
                  type="number"
                  placeholder="12"
                  value={avgUserSessionMinutes}
                  onChange={(event) => setAvgUserSessionMinutes(event.target.value)}
                />
                <p className="text-sm text-muted-foreground">
                  Time saved = average per session × user sessions
                </p>
              </div>

              {!hideCi && (
                <div className="space-y-2">
                  <div className="flex items-center justify-between gap-2">
                    <Label htmlFor="avg-ci-wait">
                      Average CI waiting time saved (minutes)
                    </Label>
                    <span className="rounded-full bg-muted px-2 py-1 text-xs font-semibold text-muted-foreground">
                      {data.totalCiSessions} CI sessions
                    </span>
                  </div>
                  <Input
                    id="avg-ci-wait"
                    type="number"
                    placeholder="8"
                    value={avgCiWaitMinutes}
                    onChange={(event) => setAvgCiWaitMinutes(event.target.value)}
                  />
                  <p className="text-sm text-muted-foreground">
                    Time saved = average wait saved × CI sessions.{" "}
                    <a
                      className="text-primary underline-offset-4 hover:underline"
                      href="https://metalbear.com/mirrord/docs/using-mirrord/mirrord-for-ci"
                      target="_blank"
                      rel="noreferrer"
                    >
                      Learn more about CI
                    </a>
                  </p>
                </div>
              )}

              <Separator />

              <div className="space-y-3">
                <h3 className="text-base font-semibold text-foreground">Preview</h3>
                <div className="grid gap-3 sm:grid-cols-2">
                  <div className="rounded-xl bg-muted px-4 py-3 text-sm text-muted-foreground">
                    User sessions: {userMinutes} min × {data.totalUserSessions} ={" "}
                    {formatHours(userHours)}h
                  </div>
                  {!hideCi && (
                    <div className="rounded-xl bg-muted px-4 py-3 text-sm text-muted-foreground">
                      CI waiting: {ciMinutes} min × {data.totalCiSessions} ={" "}
                      {formatHours(ciHours)}h
                    </div>
                  )}
                </div>
              </div>

              <div className="flex flex-col-reverse gap-3 sm:flex-row sm:justify-end">
                <Button variant="outline" onClick={() => setIsAdjustOpen(false)}>
                  Cancel
                </Button>
                <Button onClick={() => setIsAdjustOpen(false)}>
                  Save Changes
                </Button>
              </div>
            </div>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
};

export default AdminAllTimeMetrics;
