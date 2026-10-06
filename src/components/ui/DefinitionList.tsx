import type { ReactNode } from 'react';

export type DefinitionItem = {
  term: ReactNode;
  detail: ReactNode;
};

type DefinitionListProps = {
  items: DefinitionItem[];
  className?: string;
};

export default function DefinitionList({ items, className = '' }: DefinitionListProps) {
  return (
    <dl className={`definition-list ${className}`}>
      {items.map((item, idx) => (
        <div key={idx} style={{ display: 'contents' }}>
          <dt>{item.term}</dt>
          <dd>{item.detail}</dd>
        </div>
      ))}
    </dl>
  );
}
