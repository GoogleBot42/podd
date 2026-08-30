import { Box, Paper, Tab, Tabs } from '@mui/material';
import { useScheduleStore } from './scheduleStore.tsx';
import { useAppStore } from '@state/appStore.tsx';
import { LOWERCASE_DAYS } from './days.ts';

const formatDayLabel = (day: string) => `${day[0].toUpperCase()}${day.slice(1)}`;

export interface DayTabsProps {
  // When provided, tab changes are routed through this instead of jumping
  // straight to selectDay — lets the page guard against discarding unsaved
  // edits before actually switching days.
  onRequestDayChange?: (dayIndex: number) => void;
}

export default function DayTabs({ onRequestDayChange }: DayTabsProps) {
  const { selectDay, selectedDayIndex } = useScheduleStore();
  const { isUpdating } = useAppStore();

  const handleChange = (_: React.SyntheticEvent, index: number) => {
    if (onRequestDayChange) {
      onRequestDayChange(index);
    } else {
      selectDay(index);
    }
  };

  return (
    <Paper sx={ { width: '100%' } }>
      <Tabs
        value={ selectedDayIndex || 0 }
        onChange={ handleChange }
        aria-label="Days of the week"
        sx={ {
          width: '100%',
          '.MuiTabs-flexContainer': {
            display: 'flex',
            width: '100%',
          },
        } }
      >
        { LOWERCASE_DAYS.map((day, index) => (
          <Tab
            key={ index }
            disabled={ isUpdating }
            label={
              <Box
                sx={ {
                  display: 'flex',
                  justifyContent: 'center',
                  width: '100%',
                } }
              >
                { /* Mobile: 3 letters | Larger screens: full name */ }
                <Box sx={ { display: { xs: 'block', sm: 'none' } } }>
                  { formatDayLabel(day.substring(0, 3)) }
                </Box>
                <Box sx={ { display: { xs: 'none', sm: 'block' } } }>
                  { formatDayLabel(day) }
                </Box>
              </Box>
            }
            sx={ {
              flex: 1,
              minWidth: 0,
              paddingX: 1,
            } }
          />
        )) }
      </Tabs>
    </Paper>
  );
}
