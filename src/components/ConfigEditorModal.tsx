import { useEffect, useRef } from "react";
import { Copy, X, Loader2 } from "lucide-react";
import { cn } from "../utils/cn";
import { useLocale } from "../hooks/useLocale";

interface EditorState {
  app: string;
  fileName: string;
  allFiles: string[];
  content: string;
  isGenerated: boolean;
  isValid: boolean;
}

interface ConfigEditorModalProps {
  editorState: EditorState;
  appIcon: React.ReactNode;
  appName: string;
  syncing: boolean;
  onClose: () => void;
  onChange: (content: string) => void;
  onSwitchFile: (fileName: string) => void;
  onApply: () => void;
}

export default function ConfigEditorModal({
  editorState,
  appIcon,
  appName,
  syncing,
  onClose,
  onChange,
  onSwitchFile,
  onApply,
}: ConfigEditorModalProps) {
  const backdropRef = useRef<HTMLDivElement>(null);
  const modalRef = useRef<HTMLDivElement>(null);
  const { t } = useLocale();

  // ESC to close
  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  // Focus trap: keep focus inside modal
  useEffect(() => {
    const el = modalRef.current;
    if (el) {
      const focusable = el.querySelector<HTMLElement>("textarea, button, [tabindex]");
      focusable?.focus();
    }
  }, []);

  const title = editorState.isGenerated
    ? `${t("configEditor.sync")} ${appName}`
    : `${t("configEditor.viewConfig")} ${appName} ${t("configEditor.config")}`;

  return (
    <div
      ref={backdropRef}
      className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/50 backdrop-blur-sm"
      onClick={(e) => {
        if (e.target === backdropRef.current) onClose();
      }}
      role="dialog"
      aria-modal="true"
      aria-label={title}
    >
      <div
        ref={modalRef}
        className="bg-base-100 rounded-2xl shadow-2xl border border-base-300 w-full max-w-2xl overflow-hidden"
      >
        {/* Header */}
        <div className="px-6 py-4 border-b border-base-200 flex items-center justify-between bg-base-200/30">
          <div>
            <h3 className="font-bold flex items-center gap-2">
              {appIcon}
              {title}
            </h3>
            <div className="mt-2 flex gap-2">
              {editorState.allFiles.map((file) => (
                <button
                  key={file}
                  onClick={() => onSwitchFile(file)}
                  className={cn(
                    "px-3 py-1 text-[10px] font-bold rounded-lg border transition-all",
                    editorState.fileName === file
                      ? "bg-primary text-primary-content border-primary"
                      : "bg-base-200 text-base-content/50 border-base-300 hover:border-primary/30",
                  )}
                >
                  {file}
                </button>
              ))}
            </div>
          </div>
          <div className="flex items-center gap-1">
            <button
              onClick={() => navigator.clipboard.writeText(editorState.content)}
              className="btn btn-ghost btn-sm"
              title={t("common.copy")}
            >
              <Copy size={16} />
            </button>
            <button onClick={onClose} className="btn btn-ghost btn-sm">
              <X size={18} />
            </button>
          </div>
        </div>

        {/* Body */}
        <div className="p-6">
          {editorState.isGenerated ? (
            <>
              <textarea
                className={cn(
                  "textarea textarea-bordered w-full font-mono text-xs leading-relaxed min-h-[300px] resize-y",
                  !editorState.isValid && "textarea-error",
                )}
                value={editorState.content}
                onChange={(e) => onChange(e.target.value)}
              />
              {!editorState.isValid && (
                <p className="text-error text-xs mt-1">{t("common.invalidJson")}</p>
              )}
              <div className="flex justify-end gap-2 mt-4">
                <button className="btn btn-ghost btn-sm" onClick={onClose}>
                  {t("common.cancel")}
                </button>
                <button
                  className="btn btn-primary btn-sm"
                  disabled={!editorState.isValid || syncing}
                  onClick={onApply}
                >
                  {syncing ? <Loader2 size={14} className="animate-spin" /> : null}
                  {t("common.apply")}
                </button>
              </div>
            </>
          ) : (
            <div className="bg-neutral rounded-xl p-4 overflow-auto max-h-[50vh]">
              <pre className="text-xs font-mono text-neutral-content leading-relaxed whitespace-pre-wrap">
                {editorState.content}
              </pre>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
