import { useState, useEffect, Fragment } from 'react'
import {
  Button,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  Separator,
} from '@metalbear/ui'
import { Check, ExternalLink, Loader2 } from 'lucide-react'
import type { OperatorWatchStatus } from '../types'
import { strings } from '../strings'

interface ConnectOperatorModalProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  watchStatus: OperatorWatchStatus | null
}

const AUTO_CLOSE_DELAY_MS = 1500
const COPY_FEEDBACK_MS = 1500

const HELM_REPO_CMD =
  'helm repo add metalbear https://metalbear-co.github.io/charts'
const INSTALL_CMD =
  'helm install --set license.key=<YOUR_KEY> mirrord-operator metalbear/mirrord-operator'
const VERIFY_CMD = 'mirrord operator status'

export default function ConnectOperatorModal({
  open,
  onOpenChange,
  watchStatus,
}: ConnectOperatorModalProps) {
  const [step, setStep] = useState(0)

  useEffect(() => {
    if (!open) setStep(0)
  }, [open])

  useEffect(() => {
    if (open && watchStatus?.status === 'watching') {
      const t = setTimeout(() => onOpenChange(false), AUTO_CLOSE_DELAY_MS)
      return () => clearTimeout(t)
    }
    return undefined
  }, [open, watchStatus, onOpenChange])

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="!max-w-[640px] !gap-0 !p-0 sm:!max-w-[640px]">
        <DialogHeader className="px-6 pb-4 pt-6">
          <DialogTitle>{strings.operatorWizard.title}</DialogTitle>
          <DialogDescription>
            {strings.operatorWizard.subtitle}
          </DialogDescription>
          <div className="pt-4">
            <Stepper step={step} />
          </div>
        </DialogHeader>

        <Separator />

        <div className="min-h-[260px] min-w-0 overflow-hidden px-6 py-5">
          {step === 0 && <SignupBody />}
          {step === 1 && <InstallBody />}
          {step === 2 && <VerifyBody watchStatus={watchStatus} />}
        </div>

        <Separator />

        <div className="flex items-center gap-3 px-6 py-3">
          {step > 0 ? (
            <button
              type="button"
              onClick={() => setStep(step - 1)}
              className="text-muted-foreground hover:text-foreground text-xs"
            >
              {strings.operatorWizard.back}
            </button>
          ) : (
            <span />
          )}
          <div className="ml-auto flex items-center gap-3">
            {step === 0 && (
              <>
                <button
                  type="button"
                  onClick={() => setStep(1)}
                  className="text-muted-foreground hover:text-foreground text-xs"
                >
                  {strings.operatorWizard.iAlreadyHaveKey}
                </button>
                <Button asChild size="sm">
                  <a
                    href="https://app.metalbear.com/?utm_source=connect-operator-modal&utm_medium=session-monitor"
                    target="_blank"
                    rel="noreferrer"
                    onClick={() => setStep(1)}
                    className="inline-flex items-center gap-1.5"
                  >
                    {strings.operatorWizard.openApp}{' '}
                    <ExternalLink className="h-3 w-3" />
                  </a>
                </Button>
              </>
            )}
            {step === 1 && (
              <Button size="sm" onClick={() => setStep(2)}>
                {strings.operatorWizard.iveInstalledIt}
              </Button>
            )}
            {step === 2 && (
              <Button size="sm" onClick={() => onOpenChange(false)}>
                {strings.operatorWizard.done}
              </Button>
            )}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}

function Stepper({ step }: { step: number }) {
  const labels = ['Sign up', 'Install', 'Verify']
  return (
    <div className="border-border mb-4 flex items-center justify-center gap-3 border-b pb-3">
      {labels.map((label, i) => (
        <Fragment key={label}>
          <div
            className={`flex items-center gap-1.5 text-xs font-semibold ${
              step >= i ? 'text-primary' : 'text-muted-foreground'
            }`}
          >
            <span
              className={`text-meta inline-flex h-5 w-5 items-center justify-center rounded-full font-bold ${
                step > i
                  ? 'bg-primary text-primary-foreground'
                  : step === i
                    ? 'bg-primary/15 text-primary'
                    : 'bg-muted text-muted-foreground'
              }`}
            >
              {step > i ? '✓' : i + 1}
            </span>
            {label}
          </div>
          {i < labels.length - 1 && (
            <div
              className={`h-px w-12 ${step > i ? 'bg-primary' : 'bg-border'}`}
            />
          )}
        </Fragment>
      ))}
    </div>
  )
}

function SignupBody() {
  return (
    <div className="flex flex-col gap-4">
      <div>
        <h3 className="mb-1 text-sm font-bold">
          {strings.operatorWizard.signupTitle}
        </h3>
        <p className="text-muted-foreground text-xs leading-relaxed">
          {strings.operatorWizard.signupBody}
        </p>
      </div>
      <ul className="flex flex-col gap-2">
        <Bullet>{strings.operatorWizard.bulletQueue}</Bullet>
        <Bullet>{strings.operatorWizard.bulletDb}</Bullet>
        <Bullet>{strings.operatorWizard.bulletPolicy}</Bullet>
      </ul>
    </div>
  )
}

function InstallBody() {
  const rows = [
    { label: '1. Add Helm repository', cmd: HELM_REPO_CMD },
    { label: '2. Install the operator', cmd: INSTALL_CMD },
    { label: '3. Verify installation', cmd: VERIFY_CMD },
  ]
  return (
    <div className="flex min-w-0 flex-col gap-4">
      <div>
        <h3 className="mb-1 text-sm font-bold">
          {strings.operatorWizard.installTitle}
        </h3>
        <p className="text-muted-foreground text-xs leading-relaxed">
          {strings.operatorWizard.installBodyPrefix}{' '}
          <code className="text-meta bg-muted rounded px-1 font-mono">
            {strings.operatorWizard.yourKeyPlaceholder}
          </code>
          {strings.operatorWizard.installBodySuffix}
        </p>
      </div>
      <div className="flex min-w-0 flex-col gap-2">
        {rows.map((row) => (
          <CommandRow key={row.label} label={row.label} cmd={row.cmd} />
        ))}
      </div>
      <p className="text-muted-foreground text-xs">
        {strings.operatorWizard.preferIac}{' '}
        <a
          href="https://metalbear.com/mirrord/docs/managing-mirrord/operator?utm_source=connect-operator-modal&utm_medium=session-monitor"
          target="_blank"
          rel="noreferrer"
          className="text-primary hover:underline"
        >
          {strings.operatorWizard.installDocsLink}
        </a>
        {strings.common.period}
      </p>
    </div>
  )
}

function VerifyBody({
  watchStatus,
}: {
  watchStatus: OperatorWatchStatus | null
}) {
  const isWatching = watchStatus?.status === 'watching'
  return (
    <div className="flex flex-col gap-4">
      <div>
        <h3 className="mb-1 text-sm font-bold">
          {strings.operatorWizard.verifyTitle}
        </h3>
        <p className="text-muted-foreground text-xs leading-relaxed">
          {strings.operatorWizard.verifyBody}
        </p>
      </div>
      <div className="bg-card border-border flex items-center gap-3 rounded-lg border px-4 py-3">
        {isWatching ? (
          <Check className="text-primary h-5 w-5" />
        ) : (
          <Loader2 className="text-primary h-5 w-5 animate-spin" />
        )}
        <div>
          <div className="text-xs font-semibold">
            {isWatching ? 'Operator detected' : 'Watching your cluster…'}
          </div>
          <div className="text-meta text-muted-foreground">
            {isWatching
              ? 'Closing this dialog now.'
              : 'Polling every 5s for the operator CRD'}
          </div>
        </div>
      </div>
      <p className="text-muted-foreground text-xs">
        {strings.operatorWizard.stuck}{' '}
        <a
          href="https://metalbear.com/mirrord/docs/troubleshooting/?utm_source=connect-operator-modal&utm_medium=session-monitor"
          target="_blank"
          rel="noreferrer"
          className="text-primary hover:underline"
        >
          {strings.operatorWizard.troubleshootingLink}
        </a>
      </p>
    </div>
  )
}

function CommandRow({ label, cmd }: { label: string; cmd: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="min-w-0 max-w-full">
      <div className="text-meta text-muted-foreground mb-1 font-semibold">
        {label}
      </div>
      <div
        className="relative rounded-lg bg-zinc-900"
        style={{ maxWidth: '100%' }}
      >
        <pre
          className="m-0 overflow-x-auto whitespace-nowrap rounded-lg p-3 pr-16 font-mono text-xs leading-relaxed text-zinc-100"
          style={{ maxWidth: '100%' }}
        >
          {cmd}
        </pre>
        <button
          type="button"
          onClick={() => {
            const clipboard = (navigator as { clipboard?: Clipboard }).clipboard
            if (clipboard) void clipboard.writeText(cmd)
            setCopied(true)
            setTimeout(() => setCopied(false), COPY_FEEDBACK_MS)
          }}
          className="text-meta absolute right-1.5 top-1.5 rounded border border-zinc-700 bg-zinc-800 px-2 py-1 font-semibold text-zinc-100 hover:bg-zinc-700"
        >
          {copied ? 'Copied' : 'Copy'}
        </button>
      </div>
    </div>
  )
}

function Bullet({ children }: { children: React.ReactNode }) {
  return (
    <li className="flex items-start gap-2 text-xs">
      <span className="bg-primary/15 text-primary text-caps mt-0.5 inline-flex h-4 w-4 flex-shrink-0 items-center justify-center rounded-full font-bold">
        {strings.operatorWizard.checkmark}
      </span>
      <span>{children}</span>
    </li>
  )
}
