/* global db, rs, sleep */

const databaseName = process.env.MONGO_INITDB_DATABASE || "manchester_dnd";
const admin = db.getSiblingDB("admin");

try {
  rs.status();
} catch (error) {
  if (error.code !== 94 && error.codeName !== "NotYetInitialized") {
    throw error;
  }
  rs.initiate({
    _id: "rs0",
    members: [{ _id: 0, host: "mongodb:27017" }],
  });
}

for (let attempt = 0; attempt < 120; attempt += 1) {
  if (db.hello().isWritablePrimary === true) {
    break;
  }
  if (attempt === 119) {
    throw new Error("replica set did not become primary");
  }
  sleep(250);
}

const appRole = "manchesterAppRole";
const appPrivileges = [
  {
    resource: { db: databaseName, collection: "" },
    actions: ["find", "insert", "remove", "update", "listCollections", "listIndexes"],
  },
];
if (admin.getRole(appRole) === null) {
  admin.createRole({ role: appRole, privileges: appPrivileges, roles: [] });
} else {
  admin.updateRole(appRole, { privileges: appPrivileges, roles: [] });
}

function ensureUser(username, password, roles) {
  if (!username || !password) {
    throw new Error("MongoDB bootstrap usernames and passwords must be non-empty");
  }
  if (admin.getUser(username) === null) {
    admin.createUser({ user: username, pwd: password, roles });
  } else {
    admin.updateUser(username, { pwd: password, roles });
  }
}

ensureUser(process.env.MONGO_APP_USERNAME, process.env.MONGO_APP_PASSWORD, [
  { role: appRole, db: "admin" },
]);
ensureUser(process.env.MONGO_SCHEMA_USERNAME, process.env.MONGO_SCHEMA_PASSWORD, [
  { role: "dbOwner", db: databaseName },
]);
