//! Test-only candidate comparison; no event conversion or recurrence interpretation.

#[derive(Clone, Copy, Debug)]
pub enum Candidate {
    Ical,
    Icalendar,
}

pub const CANDIDATES: [Candidate; 2] = [Candidate::Ical, Candidate::Icalendar];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub value: String,
    pub params: Vec<(String, Vec<String>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub properties: Vec<Property>,
    pub components: Vec<Component>,
}

impl Component {
    pub fn property(&self, name: &str) -> Option<&Property> {
        self.properties.iter().find(|p| p.name == name)
    }
}

impl Property {
    pub fn parameter(&self, name: &str) -> Option<&[String]> {
        self.params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_slice())
    }
}

/// Preserve candidate values without domain interpretation or TEXT fixes.
/// `ical` groups component kinds, so cross-kind source order is not represented.
pub fn parse(candidate: Candidate, input: &str) -> Result<Component, String> {
    match candidate {
        Candidate::Ical => parse_ical(input),
        Candidate::Icalendar => {
            let unfolded = icalendar::parser::unfold(input);
            let mut roots = icalendar::parser::read_components(&unfolded)?;
            if roots.len() != 1 || roots[0].name.as_str() != "VCALENDAR" {
                return Err("evaluation expects one VCALENDAR".into());
            }
            Ok(from_icalendar(roots.remove(0)))
        }
    }
}

fn from_icalendar(component: icalendar::parser::Component<'_>) -> Component {
    Component {
        name: component.name.to_string(),
        properties: component
            .properties
            .into_iter()
            .map(|p| Property {
                name: p.name.to_string(),
                value: p.val.to_string(),
                params: p
                    .params
                    .into_iter()
                    .map(|p| {
                        (
                            p.key.to_string(),
                            p.val.into_iter().map(|v| v.to_string()).collect(),
                        )
                    })
                    .collect(),
            })
            .collect(),
        components: component
            .components
            .into_iter()
            .map(from_icalendar)
            .collect(),
    }
}

fn from_ical_properties(properties: Vec<ical::property::Property>) -> Vec<Property> {
    properties
        .into_iter()
        .map(|p| Property {
            name: p.name,
            value: p.value.unwrap_or_default(),
            params: p.params.unwrap_or_default(),
        })
        .collect()
}

fn ical_component(
    name: &str,
    properties: Vec<ical::property::Property>,
    components: Vec<Component>,
) -> Component {
    Component {
        name: name.into(),
        properties: from_ical_properties(properties),
        components,
    }
}

fn ical_alarms(alarms: Vec<ical::parser::ical::component::IcalAlarm>) -> Vec<Component> {
    alarms
        .into_iter()
        .map(|a| ical_component("VALARM", a.properties, vec![]))
        .collect()
}

fn parse_ical(input: &str) -> Result<Component, String> {
    let mut parser = ical::IcalParser::new(std::io::Cursor::new(input.as_bytes()));
    let calendar = parser
        .next()
        .ok_or("evaluation expects one VCALENDAR")?
        .map_err(|e| e.to_string())?;
    if parser.next().is_some() {
        return Err("evaluation expects one VCALENDAR".into());
    }
    let mut children = Vec::new();
    for timezone in calendar.timezones {
        let transitions = timezone
            .transitions
            .into_iter()
            .map(|t| {
                use ical::parser::ical::component::IcalTimeZoneTransitionType;
                let name = match t.transition {
                    IcalTimeZoneTransitionType::STANDARD => "STANDARD",
                    IcalTimeZoneTransitionType::DAYLIGHT => "DAYLIGHT",
                };
                ical_component(name, t.properties, vec![])
            })
            .collect();
        children.push(ical_component(
            "VTIMEZONE",
            timezone.properties,
            transitions,
        ));
    }
    for event in calendar.events {
        children.push(ical_component(
            "VEVENT",
            event.properties,
            ical_alarms(event.alarms),
        ));
    }
    children.extend(ical_alarms(calendar.alarms));
    for todo in calendar.todos {
        children.push(ical_component(
            "VTODO",
            todo.properties,
            ical_alarms(todo.alarms),
        ));
    }
    for journal in calendar.journals {
        children.push(ical_component("VJOURNAL", journal.properties, vec![]));
    }
    for free_busy in calendar.free_busys {
        children.push(ical_component("VFREEBUSY", free_busy.properties, vec![]));
    }
    Ok(ical_component("VCALENDAR", calendar.properties, children))
}
