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
import { useAppStore } from '@state/appStore.tsx';
import { useSchedules } from '@api/schedules';
import { useScheduleStore } from './scheduleStore.tsx';
import { useSettings } from '@api/settings';
import { LOWERCASE_DAYS } from './days.ts';
import TemperatureScheduleChart from './ScheduleChart.tsx';
import ErrorBoundary from '@components/ErrorBoundary.tsx';


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
  const { setIsUpdating, side } = useAppStore();
  const { data: schedules, refetch } = useSchedules();
  const {
    selectedSchedule,
    setOriginalSchedules,
    selectedDays,
    selectedDay,
    reloadScheduleData,
    selectDay
  } = useScheduleStore();
  const { data: settings } = useSettings();
  const [saveError, setSaveError] = useState(false);
  const displayCelsius = settings?.temperatureFormat === 'celsius';
  // TODO: Add changes lost notification using changesPresent when user tries to switch tab before saving

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
      <SideControl/>

      <DayTabs/>
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
    </PageContainer>
  );
}
