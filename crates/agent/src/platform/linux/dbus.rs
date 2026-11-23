//! Connections use the session's own bus address, which may differ from the process environment.

use anyhow::{anyhow, Context, Result};
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::connection::Builder;
use zbus::blocking::{Connection, MessageIterator};
use zbus::match_rule::MatchRule;
use zbus::message::Type;

use super::session::SessionEnv;

pub fn builder(env: &SessionEnv) -> Result<Builder<'static>> {
    let address = env.dbus_address().ok_or_else(|| anyhow!("no session bus address"))?;
    Ok(Builder::address(address.as_str())?)
}

pub fn connect(env: &SessionEnv) -> Result<Connection> {
    builder(env)?.build().context("cannot connect to the session bus")
}

#[derive(Debug, PartialEq, Eq)]
pub enum Notice {
    Signal,
    Gone,
}

pub struct Watch {
    messages: MessageIterator,
    name: &'static str,
}

pub struct SignalSpec {
    pub path: &'static str,
    pub interface: &'static str,
    pub member: &'static str,
}

impl Watch {
    /// Errors if `name` has no owner right now. The subscription is in place before the owner is
    /// checked, so a service that disappears afterwards is never missed.
    pub fn new(conn: &Connection, name: &'static str, signal: Option<SignalSpec>) -> Result<Watch> {
        let bus = DBusProxy::new(conn)?;
        let owner_changed = MatchRule::builder()
            .msg_type(Type::Signal)
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .add_arg(name)?
            .build();
        bus.add_match_rule(owner_changed.into())?;
        if let Some(s) = signal {
            let rule = MatchRule::builder()
                .msg_type(Type::Signal)
                .sender(name)?
                .path(s.path)?
                .interface(s.interface)?
                .member(s.member)?
                .build();
            bus.add_match_rule(rule.into())?;
        }
        let messages = MessageIterator::from(conn);
        if !bus.name_has_owner(name.try_into()?)? {
            return Err(anyhow!("{name} is not on the bus"));
        }
        Ok(Watch { messages, name })
    }

    pub fn next(&mut self) -> Notice {
        for message in self.messages.by_ref() {
            let Ok(message) = message else { return Notice::Gone };
            let header = message.header();
            if header.message_type() != Type::Signal {
                continue;
            }
            if header.member().is_some_and(|m| m.as_str() == "NameOwnerChanged") {
                if let Ok((name, _old, new)) = message.body().deserialize::<(String, String, String)>() {
                    if name == self.name && new.is_empty() {
                        return Notice::Gone;
                    }
                }
            } else {
                return Notice::Signal;
            }
        }
        Notice::Gone
    }
}
