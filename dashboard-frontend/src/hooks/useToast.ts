import { useState, useCallback } from 'react';

export type ToastVariant = 'default' | 'success' | 'destructive';

export interface ToastData {
  id: string;
  title: string;
  description?: string;
  variant: ToastVariant;
}

export function useToast() {
  const [toasts, setToasts] = useState<ToastData[]>([]);

  const toast = useCallback(
    ({
      title,
      description,
      variant = 'default',
    }: {
      title: string;
      description?: string;
      variant?: ToastVariant;
    }) => {
      const id = Math.random().toString(36).substring(2, 9);
      setToasts((prev) => [...prev, { id, title, description, variant }]);

      // Auto-dismiss after 4 seconds
      setTimeout(() => {
        setToasts((prev) => prev.filter((t) => t.id !== id));
      }, 4000);

      return id;
    },
    []
  );

  const dismiss = useCallback((id: string) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  const success = useCallback(
    (title: string, description?: string) => {
      return toast({ title, description, variant: 'success' });
    },
    [toast]
  );

  const error = useCallback(
    (title: string, description?: string) => {
      return toast({ title, description, variant: 'destructive' });
    },
    [toast]
  );

  return {
    toasts,
    toast,
    dismiss,
    success,
    error,
  };
}
