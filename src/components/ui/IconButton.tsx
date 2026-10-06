import type { ButtonHTMLAttributes, ReactNode } from 'react';

type IconButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  title: string;
  icon: ReactNode;
  variant?: 'default' | 'danger';
};

export default function IconButton({
  title,
  icon,
  variant = 'default',
  className = '',
  ...props
}: IconButtonProps) {
  const baseClass = variant === 'danger' ? 'danger-icon-button' : 'icon-button';
  return (
    <button
      type="button"
      className={`${baseClass} ${className}`}
      title={title}
      aria-label={title}
      {...props}
    >
      {icon}
    </button>
  );
}
