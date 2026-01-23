import {
  ToastProvider,
  ToastViewport,
  Toast,
  ToastTitle,
  ToastDescription,
  ToastClose,
} from '@metalbear/ui';
import type { ToastData } from '@/hooks/useToast';

interface ToasterProps {
  toasts: ToastData[];
  onDismiss: (id: string) => void;
}

export function Toaster({ toasts, onDismiss }: ToasterProps) {
  const getToastClassName = (variant: string) => {
    if (variant === 'success') {
      return 'border-primary bg-[#E4E3FD] text-[#232141] dark:bg-[#232141] dark:text-[#E4E3FD] dark:border-primary';
    }
    return undefined;
  };

  return (
    <ToastProvider>
      {toasts.map((t) => (
        <Toast
          key={t.id}
          variant={t.variant === 'success' ? 'default' : t.variant}
          className={getToastClassName(t.variant)}
          onOpenChange={(open: boolean) => !open && onDismiss(t.id)}
        >
          <div className="grid gap-1">
            <ToastTitle>{t.title}</ToastTitle>
            {t.description && <ToastDescription>{t.description}</ToastDescription>}
          </div>
          <ToastClose />
        </Toast>
      ))}
      <ToastViewport />
    </ToastProvider>
  );
}
