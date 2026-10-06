import { useState, useCallback } from 'react';
import { Copy, Check } from 'lucide-react';

type CopyButtonProps = {
  text: string;
  title?: string;
  className?: string;
};

export default function CopyButton({ text, title = 'Copy to clipboard', className = '' }: CopyButtonProps) {
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // ignore
    }
  }, [text]);

  return (
    <button
      type="button"
      className={`icon-button ${className}`}
      title={copied ? 'Copied!' : title}
      aria-label={copied ? 'Copied' : title}
      onClick={handleCopy}
    >
      {copied ? <Check size={13} className="status-ok" /> : <Copy size={13} />}
    </button>
  );
}
