import {
  Accordion,
  AccordionSummary,
  Box,
  Button,
  Checkbox,
  FormControlLabel,
  FormGroup,
  Typography
} from '@mui/material';
import ExpandMoreIcon from '@mui/icons-material/ExpandMore';
import { AccordionExpanded } from './SchedulePage.types.ts';
import { DayOfWeek } from '@api/schedulesSchema.ts';
import { useAppStore } from '@state/appStore.tsx';
import { useScheduleStore } from './scheduleStore';
import { LOWERCASE_DAYS } from './days.ts';
import EventRepeatIcon from '@mui/icons-material/EventRepeat';

export const daysOfWeek = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];

const WEEKDAYS: DayOfWeek[] = ['monday', 'tuesday', 'wednesday', 'thursday', 'friday'];
const WEEKENDS: DayOfWeek[] = ['saturday', 'sunday'];

const ACCORDION_NAME: AccordionExpanded = 'applyToDays';

export default function ApplyToOtherDaysAccordion() {
  const {
    selectedDays,
    selectDays,
    toggleSelectedDay,
    accordionExpanded,
    setAccordionExpanded,
    selectedSchedule,
  } = useScheduleStore();
  const { isUpdating } = useAppStore();

  return (
    <Accordion
      sx={ { width: '100%', mt: -2 } }
      expanded={ accordionExpanded === ACCORDION_NAME }
      onChange={ () => setAccordionExpanded(ACCORDION_NAME) }
      disabled={ !selectedSchedule?.power.enabled }

    >
      <AccordionSummary expandIcon={ <ExpandMoreIcon/> }>
        <Typography sx={ { display: 'flex', alignItems: 'center', gap: 3 } }>
          <EventRepeatIcon /> Apply settings to other days
        </Typography>
      </AccordionSummary>
      <Box sx={ { mt: -2, p: 2 } }>
        <Box sx={ { display: 'flex', gap: 1 } }>
          <Button variant="contained" sx={ { mb: 1 } } onClick={ () => selectDays(WEEKDAYS) } >Weekdays</Button>
          <Button variant="contained" sx={ { mb: 1 } } onClick={ () => selectDays(WEEKENDS) }>Weekends</Button>
          <Button variant="contained" sx={ { mb: 1 } } onClick={ () => selectDays(LOWERCASE_DAYS) }>Everyday</Button>
        </Box>
        <FormGroup>
          {
            daysOfWeek.map((day) => {
              const lowerCaseDay = day.toLowerCase() as DayOfWeek;
              return (
                <FormControlLabel
                  key={ day }
                  control={
                    <Checkbox
                      disabled={ isUpdating }
                      checked={ selectedDays[lowerCaseDay] }
                      onChange={ () => toggleSelectedDay(lowerCaseDay) }
                    />
                  }
                  label={ day }
                />
              );
            })
          }
        </FormGroup>
      </Box>
    </Accordion>
  );
}
