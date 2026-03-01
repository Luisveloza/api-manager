import { AlertTriangle, RefreshCw, X } from "lucide-react";
import { useLocale } from "../hooks/useLocale";

interface ErrorAlertProps {
  message: string;
  onRetry?: () => void;
  onDismiss?: () => void;
}

export default function ErrorAlert({ message, onRetry, onDismiss }: ErrorAlertProps) {
  const { t } = useLocale();

  return (
    <div role="alert" className="alert alert-error">
      <AlertTriangle size={16} />
      <span className="flex-1">{message}</span>
      <div className="flex gap-1">
        {onRetry && (
          <button className="btn btn-ghost btn-xs gap-1" onClick={onRetry}>
            <RefreshCw size={12} />
            {t("common.retry")}
          </button>
        )}
        {onDismiss && (
          <button className="btn btn-ghost btn-xs" onClick={onDismiss}>
            <X size={14} />
          </button>
        )}
      </div>
    </div>
  );
}
