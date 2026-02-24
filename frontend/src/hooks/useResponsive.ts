import { useEffect } from 'react';
import { useUiStore } from '../stores/ui';

/**
 * Manages responsive sidebar behavior based on viewport width.
 * - < 768px: sidebar hidden, hamburger toggle in TopBar
 * - 768px-1199px: sidebar collapsed to icon rail
 * - >= 1200px: sidebar fully expanded
 */
export function useResponsive() {
  const setSidebarCollapsed = useUiStore((s) => s.setSidebarCollapsed);

  useEffect(() => {
    function handleResize() {
      const width = window.innerWidth;
      if (width < 768) {
        // Mobile: sidebar fully hidden (controlled by mobileMenuOpen)
        setSidebarCollapsed(false);
      } else if (width < 1200) {
        // Tablet: icon rail
        setSidebarCollapsed(true);
      } else {
        // Desktop: full sidebar
        setSidebarCollapsed(false);
      }
    }

    handleResize();
    window.addEventListener('resize', handleResize);
    return () => window.removeEventListener('resize', handleResize);
  }, [setSidebarCollapsed]);
}
