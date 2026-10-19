import { useEffect } from "react";
import { Button } from "./ui";

/** A modal asking to confirm a destructive action. */
export function Confirm({
  title,
  body,
  action,
  onConfirm,
  onClose,
}: {
  title: string;
  body: string;
  action: string;
  onConfirm: () => void;
  onClose: () => void;
}) {
  useEffect(() => {
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [onClose]);
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-zinc-950/40 p-4 backdrop-blur-[2px]" onClick={onClose}>
      <div
        role="dialog"
        aria-modal="true"
        className="w-full max-w-sm rounded-xl border border-zinc-200 bg-white p-5 shadow-xl dark:border-zinc-800 dark:bg-zinc-900"
        onClick={(e) => e.stopPropagation()}
      >
        <h3 className="text-sm font-semibold">{title}</h3>
        <p className="mt-2 text-sm text-zinc-600 dark:text-zinc-400">{body}</p>
        <div className="mt-5 flex justify-end gap-2">
          <Button onClick={onClose}>Keep it</Button>
          <Button
            autoFocus
            className="bg-red-600 text-white hover:bg-red-500"
            variant="primary"
            onClick={() => {
              onConfirm();
              onClose();
            }}
          >
            {action}
          </Button>
        </div>
      </div>
    </div>
  );
}
