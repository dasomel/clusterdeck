import { useState, useRef, type ReactNode, type KeyboardEvent } from 'react';

export type TabItem = {
  id: string;
  label: ReactNode;
  content: ReactNode;
};

type TabsProps = {
  tabs: TabItem[];
  activeTab?: string;
  onChange?: (tabId: string) => void;
  className?: string;
};

export default function Tabs({ tabs, activeTab, onChange, className = '' }: TabsProps) {
  const [internalActive, setInternalActive] = useState(tabs[0]?.id ?? '');
  const currentTab = activeTab ?? internalActive;
  const tabRefs = useRef<Record<string, HTMLButtonElement | null>>({});

  const handleSelect = (id: string) => {
    if (onChange) {
      onChange(id);
    } else {
      setInternalActive(id);
    }
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLButtonElement>, index: number) => {
    let nextIndex = index;
    if (e.key === 'ArrowRight') {
      nextIndex = (index + 1) % tabs.length;
    } else if (e.key === 'ArrowLeft') {
      nextIndex = (index - 1 + tabs.length) % tabs.length;
    } else if (e.key === 'Home') {
      nextIndex = 0;
    } else if (e.key === 'End') {
      nextIndex = tabs.length - 1;
    } else {
      return;
    }

    e.preventDefault();
    const nextTab = tabs[nextIndex];
    if (nextTab) {
      handleSelect(nextTab.id);
      tabRefs.current[nextTab.id]?.focus();
    }
  };

  const activeContent = tabs.find((t) => t.id === currentTab)?.content;

  return (
    <div className={`tabs-container ${className}`}>
      <div className="tab-list" role="tablist" aria-orientation="horizontal">
        {tabs.map((tab, idx) => {
          const isSelected = tab.id === currentTab;
          return (
            <button
              key={tab.id}
              ref={(el) => { tabRefs.current[tab.id] = el; }}
              role="tab"
              aria-selected={isSelected}
              aria-controls={`tabpanel-${tab.id}`}
              id={`tab-${tab.id}`}
              tabIndex={isSelected ? 0 : -1}
              className="tab-button"
              onClick={() => handleSelect(tab.id)}
              onKeyDown={(e) => handleKeyDown(e, idx)}
            >
              {tab.label}
            </button>
          );
        })}
      </div>

      {tabs.map((tab) => {
        const isSelected = tab.id === currentTab;
        return (
          <div
            key={tab.id}
            role="tabpanel"
            id={`tabpanel-${tab.id}`}
            aria-labelledby={`tab-${tab.id}`}
            hidden={!isSelected}
          >
            {isSelected && tab.content}
          </div>
        );
      })}
    </div>
  );
}
