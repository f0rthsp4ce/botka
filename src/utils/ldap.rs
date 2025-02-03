use std::pin::Pin;
use std::task::Poll;

use anyhow::{Context, Result};
use futures::{Stream, StreamExt};
use ldap_rs::{
    Attribute, Attributes, LdapClient, ModifyRequest, SearchEntries,
    SearchRequest,
};
use teloxide::types::UserId;

use super::ResultExt;
use crate::config::Ldap;

/// Macro to create an LDAP attribute.
macro_rules! attr {
    ($name:expr, $value:expr) => {{
        let bytes = bytes::Bytes::from($value.to_string().clone());
        Attribute { name: $name.to_string(), values: vec![bytes] }
    }};
}

/// Macro to add an optional attribute to the list.
///
/// If the value is `Some`, the attribute is added to the list.
macro_rules! optional_attr {
    ($attrs:expr, $name:expr, $value:expr) => {{
        if let Some(value) = $value {
            $attrs.push(attr!($name, value));
        }
    }};
}

/// Connect to LDAP.
pub async fn connect(
    ldap_config: &Ldap,
) -> Result<LdapClient, ldap_rs::error::Error> {
    let mut builder = LdapClient::builder(&ldap_config.domain);
    if let Some(port) = ldap_config.port {
        builder = builder.port(port);
    }
    if let Some(tls) = ldap_config.tls {
        let tls_options = if tls {
            let mut tls_options = ldap_rs::TlsOptions::tls();
            if let Some(verify_cert) = ldap_config.verify_cert {
                tls_options = tls_options.verify_certs(verify_cert);
            }
            tls_options
        } else {
            ldap_rs::TlsOptions::plain()
        };
        builder = builder.tls_options(tls_options);
    }

    let mut ldap = builder.connect().await?;
    ldap.simple_bind(&ldap_config.user, &ldap_config.password).await?;

    Ok(ldap)
}

/// Find attribute with name and return the first value as a string.
fn get_attribute_one_str<'a>(
    attrs: &'a [Attribute],
    name: &str,
) -> Result<&'a str> {
    std::str::from_utf8(
        attrs
            .iter()
            .find(|attr| attr.name == name)
            .ok_or_else(|| anyhow::anyhow!("missing telegram_id"))?
            .values
            .first()
            .ok_or_else(|| anyhow::anyhow!("empty telegram_id list"))?,
    )
    .context("invalid utf8 string")
}

/// LDAP user.
#[derive(Clone, Debug)]
pub struct User {
    pub dn: String,
    pub uid: String,
    pub cn: String,
    pub sn: String,
    pub display_name: Option<String>,
    pub telegram_id: Option<UserId>,
    pub mail: Option<String>,
    /// Hashed password in LDAP format.
    pub password: Option<String>,
}

impl User {
    pub fn new_from_telegram(
        config: &Ldap,
        telegram_id: UserId,
        username: &str,
        email: &str,
        display_name: Option<String>,
    ) -> Self {
        Self {
            dn: format!(
                "cn={},{},{}",
                username, config.users_dn, config.base_dn
            ),
            uid: username.to_string(),
            cn: username.to_string(),
            sn: username.to_string(),
            display_name,
            telegram_id: Some(telegram_id),
            mail: Some(email.to_string()),
            password: None,
        }
    }

    pub fn update_password(&mut self, algo: impl PasswordHash, password: &str) {
        self.password = Some(algo.hash_password(password));
    }
}

/// LDAP group.
#[derive(Clone, Debug)]
pub struct Group {
    pub dn: String,
    pub cn: String,
}

#[allow(dead_code)]
pub type UserGroups = Vec<String>;

/// Trait to convert a LDAP attributes into a type.
pub trait FromAttributes: Sized {
    /// Convert LDAP attributes into a type.
    fn from_attributes(
        config: &Ldap,
        dn: String,
        attributes: Attributes,
    ) -> Result<Self>;
}

impl FromAttributes for User {
    fn from_attributes(
        config: &Ldap,
        dn: String,
        attributes: Attributes,
    ) -> Result<Self> {
        let telegram_id: Option<UserId> = match get_attribute_one_str(
            &attributes,
            &config.attributes.telegram_id,
        )
        .log_ok(module_path!(), "failed to get telegram_id")
        {
            Some(id) => {
                Some(UserId(id.parse().context("failed to parse telegram id")?))
            }
            None => None,
        };
        let user_password = get_attribute_one_str(&attributes, "userPassword")
            .ok()
            .map(|s| s.to_string());
        let mail = get_attribute_one_str(&attributes, "mail")
            .ok()
            .map(|s| s.to_string());
        let display_name = get_attribute_one_str(&attributes, "displayName")
            .ok()
            .map(|s| s.to_string());
        Ok(Self {
            dn,
            uid: get_attribute_one_str(&attributes, "uid")?.to_string(),
            cn: get_attribute_one_str(&attributes, "cn")?.to_string(),
            sn: get_attribute_one_str(&attributes, "sn")?.to_string(),
            password: user_password,
            display_name,
            telegram_id,
            mail,
        })
    }
}

impl FromAttributes for Group {
    fn from_attributes(
        _config: &Ldap,
        dn: String,
        attributes: Attributes,
    ) -> Result<Self> {
        Ok(Self {
            dn,
            cn: get_attribute_one_str(&attributes, "cn")?.to_string(),
        })
    }
}

/// Trait to convert a type into LDAP attributes.
pub trait IntoAttributes: Send + Sync {
    /// Convert the type into LDAP attributes.
    fn into_attributes(
        self,
        config: &Ldap,
    ) -> impl Iterator<Item = Attribute> + Send + Sync;
}

impl IntoAttributes for User {
    fn into_attributes(self, config: &Ldap) -> impl Iterator<Item = Attribute> {
        let mut attrs = vec![
            attr!("objectClass", config.attributes.user_class),
            attr!("uid", self.uid),
            attr!("cn", self.cn),
            attr!("sn", self.sn),
        ];
        optional_attr!(attrs, &config.attributes.telegram_id, self.telegram_id);
        optional_attr!(attrs, "mail", &self.mail);
        optional_attr!(attrs, "userPassword", &self.password);
        optional_attr!(attrs, "displayName", &self.display_name);
        attrs.into_iter()
    }
}

impl IntoAttributes for Group {
    fn into_attributes(
        self,
        _config: &Ldap,
    ) -> impl Iterator<Item = Attribute> {
        vec![attr!("objectClass", "groupOfUniqueNames"), attr!("cn", self.cn)]
            .into_iter()
    }
}

impl IntoAttributes for Vec<Attribute> {
    fn into_attributes(
        self,
        _config: &Ldap,
    ) -> impl Iterator<Item = Attribute> {
        self.into_iter()
    }
}

impl IntoAttributes for Attribute {
    fn into_attributes(self, _config: &Ldap) -> impl Iterator<Item = Self> {
        std::iter::once(self)
    }
}

/// Trait to extract the DN from a type.
pub trait ExtractDn: Send + Sync {
    /// Extract the DN from the type.
    fn extract_dn(&self) -> &str;
}

impl ExtractDn for User {
    fn extract_dn(&self) -> &str {
        &self.dn
    }
}

impl ExtractDn for Group {
    fn extract_dn(&self) -> &str {
        &self.dn
    }
}

impl<T: ExtractDn> ExtractDn for &T {
    fn extract_dn(&self) -> &str {
        (*self).extract_dn()
    }
}

impl ExtractDn for String {
    fn extract_dn(&self) -> &str {
        self
    }
}

impl ExtractDn for &str {
    fn extract_dn(&self) -> &str {
        self
    }
}

/// Trait to hash a password.
pub trait PasswordHash {
    /// Hash a password.
    fn hash_password(&self, password: &str) -> String;
}

pub struct Sha512PasswordHash;

impl Sha512PasswordHash {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for Sha512PasswordHash {
    fn default() -> Self {
        Self::new()
    }
}

impl PasswordHash for Sha512PasswordHash {
    fn hash_password(&self, password: &str) -> String {
        use sha_crypt::{sha512_simple, Sha512Params};
        let params = Sha512Params::new(10_000)
            .expect("failed to create sha512 hashing params");
        let hashed_password =
            sha512_simple(password, &params).expect("failed to hash password");
        format!("{{CRYPT}}{hashed_password}")
    }
}

/// Stream of LDAP entries lazily converted to a type.
pub struct ConvertedSearchEntries<'a, T: FromAttributes> {
    stream: SearchEntries,
    config: &'a Ldap,
    _phantom: std::marker::PhantomData<T>,
}

impl<'a, T: FromAttributes> ConvertedSearchEntries<'a, T> {
    pub const fn new(stream: SearchEntries, config: &'a Ldap) -> Self {
        Self { stream, config, _phantom: std::marker::PhantomData }
    }
}

impl<T: FromAttributes> Unpin for ConvertedSearchEntries<'_, T> {}

impl<T: FromAttributes> Stream for ConvertedSearchEntries<'_, T> {
    type Item = Result<T>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let Some(entry) = futures::ready!(self.stream.poll_next_unpin(cx))
        else {
            return Poll::Ready(None);
        };
        match entry {
            Ok(entry) => Poll::Ready(Some(T::from_attributes(
                self.config,
                entry.dn,
                entry.attributes,
            ))),
            Err(e) => Poll::Ready(Some(Err(e.into()))),
        }
    }
}

/// Get LDAP entries stream lazily converted to a type.
pub async fn get<'a, T: FromAttributes + 'a>(
    ldap: &mut LdapClient,
    config: &'a Ldap,
    query: SearchRequest,
) -> Result<impl Stream<Item = Result<T>> + 'a> {
    let stream = ldap.search(query).await?;
    Ok(ConvertedSearchEntries::new(stream, config))
}

/// Get a user from LDAP by telegram ID.
pub async fn get_user(
    ldap: &mut LdapClient,
    config: &Ldap,
    user_id: UserId,
) -> Result<Option<User>> {
    let base_dn = format!("{},{}", config.users_dn, config.base_dn);
    // For some reason this query does not work.
    // let query_str = format!(
    //     "(&({telegram_id_attr}={telegram_id})(objectClass={user_class}))",
    //     telegram_id_attr = config.attributes.telegram_id,
    //     telegram_id = user_id.0,
    //     user_class = config.attributes.user_class,
    // );
    let query_str = format!(
        "({telegram_id_attr}={telegram_id})",
        telegram_id_attr = config.attributes.telegram_id,
        telegram_id = user_id.0,
    );
    let query = SearchRequest::builder()
        .scope(ldap_rs::SearchRequestScope::WholeSubtree)
        .base_dn(base_dn)
        .filter(query_str)
        .size_limit(1)
        .build()?;
    let user = match get(ldap, config, query).await?.next().await {
        Some(Ok(entries)) => entries,
        Some(Err(e)) => return Err(e),
        None => return Ok(None),
    };
    Ok(Some(user))
}

/// Add an entry to LDAP.
pub async fn add(
    ldap: &mut LdapClient,
    config: &Ldap,
    dn: impl ExtractDn,
    attrs: impl IntoAttributes,
) -> Result<()> {
    ldap.add(dn.extract_dn(), attrs.into_attributes(config)).await?;
    Ok(())
}

/// Add a user to LDAP.
pub async fn add_user(
    ldap: &mut LdapClient,
    config: &Ldap,
    user: &User,
) -> Result<()> {
    add(ldap, config, user, user.to_owned()).await
}

/// Update an entry in LDAP by replacing specified attributes.
pub async fn update_replace(
    ldap: &mut LdapClient,
    config: &Ldap,
    dn: impl ExtractDn,
    attrs: impl IntoAttributes,
) -> Result<()> {
    let mut request = ModifyRequest::builder(dn.extract_dn());
    for attr in attrs.into_attributes(config) {
        request = request.replace_op(attr);
    }
    ldap.modify(request.build()).await?;
    Ok(())
}

/// Update a user in LDAP.
pub async fn update_user(
    ldap: &mut LdapClient,
    config: &Ldap,
    user: &User,
) -> Result<()> {
    update_replace(ldap, config, user, user.to_owned()).await
}

/// Get user groups from LDAP.
#[allow(dead_code)]
pub async fn get_user_groups(
    ldap: &mut LdapClient,
    config: &Ldap,
    user: &User,
) -> Result<UserGroups> {
    let base_dn = format!("{},{}", config.groups_dn, config.base_dn);
    let user_dn = user.extract_dn();
    let query = SearchRequest::builder()
        .base_dn(base_dn)
        .filter(format!(
            "(&({member_attr}={user_dn})(objectClass={group_class}))",
            member_attr = config.attributes.group_member,
            user_dn = user_dn,
            group_class = config.attributes.group_class,
        ))
        .attributes(vec!["cn"])
        .build()?;

    let mut groups = get::<Group>(ldap, config, query).await?;

    let mut user_groups = Vec::new();

    while let Some(group) = groups.next().await {
        let group = group?;
        user_groups.push(group.cn);
    }

    Ok(user_groups)
}

/// Add a user to a group in LDAP.
pub async fn add_user_to_group(
    ldap: &mut LdapClient,
    config: &Ldap,
    user: &User,
    group: &str,
) -> Result<()> {
    let group = Group {
        dn: format!("cn={},{},{}", group, config.groups_dn, config.base_dn),
        cn: group.to_string(),
    };
    let mut request = ModifyRequest::builder(group.extract_dn());
    request = request.add_op(attr!(config.attributes.group_member, user.dn));
    ldap.modify(request.build()).await?;
    Ok(())
}

/// Remove a user from a group in LDAP.
#[allow(dead_code)]
pub async fn remove_user_from_group(
    ldap: &mut LdapClient,
    config: &Ldap,
    user: &User,
    group: &Group,
) -> Result<()> {
    let mut request = ModifyRequest::builder(group.extract_dn());
    request = request.delete_op(attr!(config.attributes.group_member, user.dn));
    ldap.modify(request.build()).await?;
    Ok(())
}
