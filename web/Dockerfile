FROM node:22-alpine
WORKDIR /app
COPY package.json ./
RUN npm install --omit=dev
COPY server.js ./
COPY trust-proxy.js ./
COPY presence.js ./
COPY public ./public
EXPOSE 8788
CMD ["node", "server.js"]
