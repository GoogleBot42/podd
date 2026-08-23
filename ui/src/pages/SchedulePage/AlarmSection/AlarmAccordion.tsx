import {
  Accordion,
  AccordionSummary,
  Box,
  Typography,
  SxProps,
} from '@mui/material';
import ExpandMoreIcon from '@mui/icons-material/ExpandMore';
import { AccordionExpanded } from '../SchedulePage.types.ts';
import { useScheduleStore } from '../scheduleStore.tsx';
import AlarmEnabledSwitch from './AlarmEnabledSwitch.tsx';
import AlarmTime from './AlarmTime.tsx';
import AlarmVibrationSlider from './AlarmVibrationSlider.tsx';
import AlarmDuration from './AlarmDuration.tsx';
import AlarmPattern from './AlarmPattern.tsx';
import React from 'react';
import AlarmIcon from '@mui/icons-material/Alarm';
import AlarmTest from './AlarmTest.tsx';

const ACCORDION_NAME: AccordionExpanded = 'alarm';

const Row = ({ children, sx }: React.PropsWithChildren<{ sx?: SxProps }>) => (
  <Box
    sx={ {
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'space-between',
      mb: 2,
      pl: 2,
      pr: 2,
      gap: 1,
      ...sx,
    } }
  >
    { children }
  </Box>
);

// eslint-disable-next-line react/no-multi-comp
export default function AlarmAccordion() {
  const {
    accordionExpanded,
    selectedSchedule,
    setAccordionExpanded,
  } = useScheduleStore();


  return (
    <Accordion
      sx={ { width: '100%', mt: -2 } }
      expanded={ accordionExpanded === ACCORDION_NAME }
      onChange={ () => setAccordionExpanded(ACCORDION_NAME) }
      disabled={ !selectedSchedule?.power.enabled }
    >
      <AccordionSummary expandIcon={ <ExpandMoreIcon/> } >
        <Box>
          <Typography sx={ { display: 'flex', alignItems: 'center', gap: 3 } }>
            <AlarmIcon /> Vibration alarm
          </Typography>
          <Typography variant='body2' color='text.secondary'>
            Not yet supported by podd — alarm settings are saved but don&apos;t
            drive the bed yet.
          </Typography>
        </Box>
      </AccordionSummary>
      <Box sx={ { width: '100%', pb: 2 } }>
        { /*
          The weekly engine drives heating only; per-weekday alarms are still a
          future item, so these controls would persist settings nothing acts on.
          Disabled here rather than in each child so there is one place to undo
          it. `inert` also keeps them out of the tab order.
          The test button below stays live: it POSTs /api/alarm (fire now) and
          never touches schedule state.
        */ }
        <Box inert sx={ { opacity: 0.5 } }>
          <Row>
            <AlarmEnabledSwitch/>
            { selectedSchedule?.alarm.enabled && <AlarmTime/> }
          </Row>
          {
            selectedSchedule?.alarm.enabled &&
            (
              <>
                <Row>
                  <AlarmDuration/>
                  <AlarmPattern/>
                </Row>
                <Row sx={ { ml: 3, mr: 3 } }>
                  <AlarmVibrationSlider/>

                </Row>
              </>
            )
          }
        </Box>
        <AlarmTest />
      </Box>
    </Accordion>
  );

}
