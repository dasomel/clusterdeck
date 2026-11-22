import { useState } from 'react';
import ConfirmModal from './ConfirmModal';

type TypedDeleteConfirmProps = {
  title: string;
  /** Step 1: what will be deleted. */
  explain: string;
  /** Step 2: the exact text the user must type. */
  confirmText: string;
  typeMessage: string;
  confirmLabel: string;
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
};

/** Irreversible deletes get two steps: explain first, then type the name to confirm. */
export default function TypedDeleteConfirm({
  title,
  explain,
  confirmText,
  typeMessage,
  confirmLabel,
  busy,
  onConfirm,
  onCancel,
}: TypedDeleteConfirmProps) {
  const [step, setStep] = useState<1 | 2>(1);

  return step === 1 ? (
    <ConfirmModal
      title={title}
      message={explain}
      confirmLabel="Continue"
      onConfirm={() => setStep(2)}
      onCancel={onCancel}
    />
  ) : (
    <ConfirmModal
      title="Confirm permanent deletion"
      message={typeMessage}
      confirmText={confirmText}
      confirmLabel={confirmLabel}
      busy={busy}
      onConfirm={onConfirm}
      onCancel={onCancel}
    />
  );
}
