import _ from 'lodash';
import { useEffect, useRef, useState } from 'react';
import { Alert, Box, Snackbar, Typography } from '@mui/material';
import { DeepPartial } from 'ts-essentials';
import moment from 'moment-timezone';

import AlarmAccordion from './AlarmSection/AlarmAccordion.tsx';
import ApplyToOtherDaysAccordion from './ApplyToOtherDaysAccordion.tsx';
import DayTabs from './DayTabs.tsx';
import EnabledSwitch from './EnabledSwitch.tsx';
import PageContainer from '../PageContainer.tsx';
import SaveButton from './SaveButton.tsx';
import SideControl from '../../components/SideControl.tsx';
import PowerScheduleSection from './PowerScheduleSection.tsx';
import TemperatureAdjustmentsAccordion from './TemperatureAdjustmentsAccordion.tsx';
import { DayOfWeek, Schedules } from '@api/schedulesSchema.ts';
import { postSchedules } from '@api/schedules';
import { Side, useAppStore } from '@state/appStore.tsx';
import { useSchedules } from '@api/schedules';
import { useScheduleStore } from './scheduleStore.tsx';
import { useSettings } from '@api/settings';
import { LOWERCASE_DAYS } from './days.ts';
import TemperatureScheduleChart from './ScheduleChart.tsx';
import ErrorBoundary from '@components/ErrorBoundary.tsx';
import UnsavedChangesDialog from './UnsavedChangesDialog.tsx';


const getAdjustedDayOfWeek = (): DayOfWeek => {
  // Get the current moment in the specified timezone
  const now = moment();
  // Extract the hour of the day in 24-hour format
  const currentHour = now.hour();

  // Determine if it's before noon (12:00 PM)
  if (currentHour < 12) {
    return now.subtract(1, 'day').format('dddd').toLocaleLowerCase() as DayOfWeek;
  } else {
    return now.format('dddd').toLocaleLowerCase() as DayOfWeek;
  }
};


export default function SchedulePage() {
  const { setIsUpdating, side, setSide } = useAppStore();
  const { data: schedules, refetch } = useSchedules();
  const {
    selectedSchedule,
    setOriginalSchedules,
    selectedDays,
    selectedDay,
    reloadScheduleData,
    selectDay,
    changesPresent,
  } = useScheduleStore();
  const { data: settings } = useSettings();
  const [saveError, setSaveError] = useState(false);
  const displayCelsius = settings?.temperatureFormat === 'celsius';

  // Switching day tabs or sides both call reloadScheduleData, which resets
  // selectedSchedule from originalSchedules with no warning — silently
  // discarding unsaved edits (#59). Route both through this guard: if there
  // are unsaved changes, stash the action and confirm before running it.
  const [pendingChange, setPendingChange] = useState<(() => void) | null>(null);

  const guardedChange = (action: () => void) => {
    if (changesPresent) {
      setPendingChange(() => action);
    } else {
      action();
    }
  };

  const handleDayChangeRequest = (dayIndex: number) => guardedChange(() => selectDay(dayIndex));
  const handleSideChangeRequest = (newSide: Side) => guardedChange(() => setSide(newSide));

  const confirmDiscard = () => {
    pendingChange?.();
    setPendingChange(null);
  };
  const cancelDiscard = () => setPendingChange(null);

  // Also warn on browser-level navigation away (tab close/refresh) — the
  // MUI dialog above only covers in-page tab/side switches.
  useEffect(() => {
    if (!changesPresent) return;
    const handleBeforeUnload = (event: BeforeUnloadEvent) => {
      event.preventDefault();
    };
    window.addEventListener('beforeunload', handleBeforeUnload);
    return () => window.removeEventListener('beforeunload', handleBeforeUnload);
  }, [changesPresent]);

  // Jumping to today is an initialisation step, not something to redo on every
  // refetch — re-running it on the post-save refetch threw the user back to
  // today's tab the moment they saved another day.
  const dayInitialized = useRef(false);

  useEffect(() => {
    if (!schedules) return;
    setOriginalSchedules(schedules);
    if (!dayInitialized.current) {
      dayInitialized.current = true;
      selectDay(LOWERCASE_DAYS.indexOf(getAdjustedDayOfWeek()));
    }
    reloadScheduleData();
  }, [schedules]);

  useEffect(() => {
    reloadScheduleData();
  }, [side]);

  const handleSave = async () => {
    setIsUpdating(true);

    const daysList: DayOfWeek[] = _.uniq(_.keys(_.pickBy(selectedDays, value => value))) as DayOfWeek[];
    daysList.push(selectedDay);
    const payload: DeepPartial<Schedules> = { [side]: {}, };
    daysList.forEach(day => {
      // @ts-expect-error
      payload[side][day] = selectedSchedule;
    });

    await postSchedules(payload)
      .then(() => {
        // Wait 1 second before refreshing the schedules
        return new Promise((resolve) => setTimeout(resolve, 1_000));
      })
      .then(() => refetch())
      .catch(error => {
        console.error(error);
        setSaveError(true);
      })
      .finally(() => {
        setIsUpdating(false);
      });
  };

  return (
    <PageContainer
      sx={ {
        width: '100%',
        maxWidth: { xs: '100%', sm: '800px' },
        mx: 'auto',
        mb: 15,
      } }
    >
      <SideControl onRequestSideChange={ handleSideChangeRequest }/>

      <DayTabs onRequestDayChange={ handleDayChangeRequest }/>
      <ErrorBoundary componentName='Scheduling chart'>
        <TemperatureScheduleChart />
      </ErrorBoundary>

      <PowerScheduleSection displayCelsius={ displayCelsius }/>
      <Box sx={ { mt: 2, display: 'flex', justifyContent: 'space-between', width: '100%', mb: 2 } }>
        <EnabledSwitch/>
        <SaveButton onSave={ handleSave }/>
      </Box>
      <Typography variant='body2' color='text.secondary' sx={ { width: '100%', mt: -1, mb: 2 } }>
        Enable a day to have the weekly schedule run this side that day; days
        left disabled stay off. If no day is enabled, the profile from
        config.ron drives this side instead.
      </Typography>
      <TemperatureAdjustmentsAccordion displayCelsius={ displayCelsius }/>
      <AlarmAccordion/>
      <ApplyToOtherDaysAccordion/>

      <Snackbar
        open={ saveError }
        autoHideDuration={ 6000 }
        onClose={ () => setSaveError(false) }
      >
        <Alert severity="error" onClose={ () => setSaveError(false) }>
          Failed to save schedule
        </Alert>
      </Snackbar>

      <UnsavedChangesDialog
        open={ pendingChange !== null }
        onDiscard={ confirmDiscard }
        onCancel={ cancelDiscard }
      />
    </PageContainer>
  );
}
