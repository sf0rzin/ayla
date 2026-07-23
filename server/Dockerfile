FROM node:22-alpine

ENV NODE_ENV=production
WORKDIR /app

COPY package.json package-lock.json ./
COPY server/package.json ./server/package.json
RUN npm ci --omit=dev --workspace @ayla/api && npm cache clean --force

COPY server/src ./server/src

USER node
EXPOSE 3000
CMD ["node", "server/src/server.js"]
