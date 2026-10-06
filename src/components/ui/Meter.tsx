type MeterProps = {
  value: number;
  max: number | null;
  warning?: boolean;
  className?: string;
  ariaLabel?: string;
};

export default function Meter({
  value,
  max,
  warning = false,
  className = '',
  ariaLabel,
}: MeterProps) {
  if (max == null || max <= 0) {
    return null;
  }

  const percentage = Math.min(100, Math.max(0, (value / max) * 100));
  const isOverflow = value > max;

  return (
    <div
      role="progressbar"
      aria-valuenow={value}
      aria-valuemin={0}
      aria-valuemax={max}
      aria-label={ariaLabel}
      className={`meter-container ${className}`}
      style={{
        position: 'relative',
        width: '100px',
        height: '6px',
        backgroundColor: 'var(--border)',
        borderRadius: 'var(--radius-sm)',
        overflow: 'hidden',
        display: 'inline-block',
        verticalAlign: 'middle',
      }}
    >
      <div
        style={{
          width: `${percentage}%`,
          height: '100%',
          backgroundColor: isOverflow || warning ? 'var(--warning)' : 'var(--accent)',
          borderRadius: 'var(--radius-sm)',
          transition: 'width 0.2s ease',
        }}
      />
    </div>
  );
}
