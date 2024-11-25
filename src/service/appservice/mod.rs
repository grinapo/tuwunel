mod namespace_regex;
mod registration_info;

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use conduit::{err, utils::stream::TryIgnore, Result};
use database::Map;
use futures::{Future, StreamExt, TryStreamExt};
use ruma::{api::appservice::Registration, RoomAliasId, RoomId, UserId};
use tokio::sync::RwLock;

pub use self::{namespace_regex::NamespaceRegex, registration_info::RegistrationInfo};
use crate::{sending, Dep};

pub struct Service {
	registration_info: RwLock<BTreeMap<String, RegistrationInfo>>,
	services: Services,
	db: Data,
}

struct Services {
	sending: Dep<sending::Service>,
}

struct Data {
	id_appserviceregistrations: Arc<Map>,
}

#[async_trait]
impl crate::Service for Service {
	fn build(args: crate::Args<'_>) -> Result<Arc<Self>> {
		Ok(Arc::new(Self {
			registration_info: RwLock::new(BTreeMap::new()),
			services: Services {
				sending: args.depend::<sending::Service>("sending"),
			},
			db: Data {
				id_appserviceregistrations: args.db["id_appserviceregistrations"].clone(),
			},
		}))
	}

	async fn worker(self: Arc<Self>) -> Result<()> {
		// Inserting registrations into cache
		for appservice in self.iter_db_ids().await? {
			self.registration_info.write().await.insert(
				appservice.0,
				appservice
					.1
					.try_into()
					.expect("Should be validated on registration"),
			);
		}

		Ok(())
	}

	fn name(&self) -> &str { crate::service::make_name(std::module_path!()) }
}

impl Service {
	/// Registers an appservice and returns the ID to the caller
	pub async fn register_appservice(&self, registration: &Registration, appservice_config_body: &str) -> Result {
		//TODO: Check for collisions between exclusive appservice namespaces
		self.registration_info
			.write()
			.await
			.insert(registration.id.clone(), registration.clone().try_into()?);

		self.db
			.id_appserviceregistrations
			.insert(&registration.id, appservice_config_body);

		Ok(())
	}

	/// Remove an appservice registration
	///
	/// # Arguments
	///
	/// * `service_name` - the registration ID of the appservice
	pub async fn unregister_appservice(&self, appservice_id: &str) -> Result<()> {
		// removes the appservice registration info
		self.registration_info
			.write()
			.await
			.remove(appservice_id)
			.ok_or(err!("Appservice not found"))?;

		// remove the appservice from the database
		self.db.id_appserviceregistrations.del(appservice_id);

		// deletes all active requests for the appservice if there are any so we stop
		// sending to the URL
		self.services
			.sending
			.cleanup_events(Some(appservice_id), None, None)
			.await
	}

	pub async fn get_registration(&self, id: &str) -> Option<Registration> {
		self.registration_info
			.read()
			.await
			.get(id)
			.cloned()
			.map(|info| info.registration)
	}

	pub async fn iter_ids(&self) -> Vec<String> {
		self.registration_info
			.read()
			.await
			.keys()
			.cloned()
			.collect()
	}

	pub async fn find_from_token(&self, token: &str) -> Option<RegistrationInfo> {
		self.read()
			.await
			.values()
			.find(|info| info.registration.as_token == token)
			.cloned()
	}

	/// Checks if a given user id matches any exclusive appservice regex
	pub async fn is_exclusive_user_id(&self, user_id: &UserId) -> bool {
		self.read()
			.await
			.values()
			.any(|info| info.is_exclusive_user_match(user_id))
	}

	/// Checks if a given room alias matches any exclusive appservice regex
	pub async fn is_exclusive_alias(&self, alias: &RoomAliasId) -> bool {
		self.read()
			.await
			.values()
			.any(|info| info.aliases.is_exclusive_match(alias.as_str()))
	}

	/// Checks if a given room id matches any exclusive appservice regex
	///
	/// TODO: use this?
	#[allow(dead_code)]
	pub async fn is_exclusive_room_id(&self, room_id: &RoomId) -> bool {
		self.read()
			.await
			.values()
			.any(|info| info.rooms.is_exclusive_match(room_id.as_str()))
	}

	pub fn read(&self) -> impl Future<Output = tokio::sync::RwLockReadGuard<'_, BTreeMap<String, RegistrationInfo>>> {
		self.registration_info.read()
	}

	#[inline]
	pub async fn all(&self) -> Result<Vec<(String, Registration)>> { self.iter_db_ids().await }

	pub async fn get_db_registration(&self, id: &str) -> Result<Registration> {
		self.db
			.id_appserviceregistrations
			.get(id)
			.await
			.and_then(|ref bytes| serde_yaml::from_slice(bytes).map_err(Into::into))
			.map_err(|e| err!(Database("Invalid appservice {id:?} registration: {e:?}")))
	}

	async fn iter_db_ids(&self) -> Result<Vec<(String, Registration)>> {
		self.db
			.id_appserviceregistrations
			.keys()
			.ignore_err()
			.then(|id: String| async move {
				let reg = self.get_db_registration(&id).await?;
				Ok((id, reg))
			})
			.try_collect()
			.await
	}
}
