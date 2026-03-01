import { useLocale } from "../hooks/useLocale";

interface PageSkeletonProps {
  message?: string;
}

export default function PageSkeleton({ message }: PageSkeletonProps) {
  const { t } = useLocale();

  return (
    <div className="space-y-6 animate-pulse">
      {/* Header skeleton */}
      <div>
        <div className="h-7 bg-base-300 rounded w-48" />
        <div className="h-4 bg-base-300 rounded w-72 mt-2" />
      </div>

      {/* Stats row skeleton */}
      <div className="grid grid-cols-2 lg:grid-cols-4 gap-4">
        {Array.from({ length: 4 }).map((_, i) => (
          <div key={i} className="h-24 bg-base-300 rounded-box" />
        ))}
      </div>

      {/* Card skeleton */}
      <div className="h-40 bg-base-300 rounded-box" />

      {/* Loading indicator */}
      <div className="flex items-center justify-center gap-2 text-base-content/40">
        <span className="loading loading-spinner loading-sm" />
        <span className="text-sm">{message ?? t("common.loading")}</span>
      </div>
    </div>
  );
}
