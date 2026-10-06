import type { ReactNode } from 'react';

type TagProps = {
  children: ReactNode;
  title?: string;
  className?: string;
};

export default function Tag({ children, title, className = '' }: TagProps) {
  return (
    <span className={`tag ${className}`} title={title}>
      {children}
    </span>
  );
}
